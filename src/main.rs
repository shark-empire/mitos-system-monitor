use eframe::egui;
use egui_plot::{Line, Plot};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sysinfo::{Pid, System};
use tokio::sync::mpsc;

use mitos_utils::ipc::{self, IpcRequest, IpcResponse, RichWidget};

// ────────────────────────── Shared state between UI thread & IPC worker ──────────────────────────

#[derive(Clone)]
pub struct TerminalSnapshot {
    pub pid: u32,
    pub prompt: String,
    pub text: String,
    pub last_seen: Instant,
}

struct SharedState {
    terminals: Vec<TerminalSnapshot>,
    last_error: Option<String>,
    last_info: Option<String>,
}

enum MonitorAction {
    Refresh,
    Inject { terminal_pid: u32, widget: RichWidget },
}

#[derive(PartialEq)]
enum Tab {
    Overview,
    Processes,
    ProcessTree,
    TerminalBuffers,
}

// ────────────────────────── IPC worker thread (tokio) ──────────────────────────

fn spawn_ipc_worker(state: Arc<Mutex<SharedState>>, mut action_rx: mpsc::Receiver<MonitorAction>) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("monitor ipc runtime");
        rt.block_on(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(1000));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = tick.tick() => refresh_terminals(&state).await,
                    action = action_rx.recv() => match action {
                        Some(MonitorAction::Refresh) => refresh_terminals(&state).await,
                        Some(MonitorAction::Inject { terminal_pid, widget }) => {
                            inject_widget(&state, terminal_pid, widget).await
                        }
                        None => break,
                    },
                }
            }
        });
    });
}

async fn refresh_terminals(state: &Arc<Mutex<SharedState>>) {
    let mut snapshots = Vec::new();
    for (_pid, path) in ipc::list_terminal_sockets() {
        match query_terminal(&path).await {
            Ok(snap) => snapshots.push(snap),
            Err(_) => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    let mut g = state.lock().unwrap();
    g.terminals = snapshots;
}

async fn query_terminal(path: &str) -> std::io::Result<TerminalSnapshot> {
    let mut stream = tokio::time::timeout(
        Duration::from_millis(500),
        tokio::net::UnixStream::connect(path),
    )
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timeout"))??;

    ipc::ipc_send(&mut stream, &IpcRequest::GetTerminalBuffer).await?;
    match ipc::ipc_recv::<IpcResponse>(&mut stream).await? {
        Some(IpcResponse::BufferData { pid, prompt, text }) => Ok(TerminalSnapshot {
            pid,
            prompt,
            text,
            last_seen: Instant::now(),
        }),
        Some(other) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unexpected response: {other:?}"),
        )),
        None => Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "stream closed",
        )),
    }
}

async fn inject_widget(state: &Arc<Mutex<SharedState>>, pid: u32, widget: RichWidget) {
    let path = ipc::terminal_socket(pid);
    let result = async {
        let mut stream = tokio::net::UnixStream::connect(&path).await?;
        ipc::ipc_send(&mut stream, &IpcRequest::InjectWidget { widget }).await?;
        ipc::ipc_recv::<IpcResponse>(&mut stream).await?;
        Ok::<(), std::io::Error>(())
    }
    .await;

    let mut g = state.lock().unwrap();
    match result {
        Ok(()) => {
            g.last_error = None;
            g.last_info = Some(format!("Widget injected into terminal PID {pid}"));
        }
        Err(e) => {
            g.last_info = None;
            g.last_error = Some(format!("Inject into PID {pid} failed: {e}"));
        }
    }
}

// ────────────────────────── Process Tree data model ──────────────────────────

struct ProcessTreeNode {
    pid: u32,
    name: String,
    cpu: f32,
    mem: u64,
    children: Vec<usize>,
    parent: Option<usize>,
    is_mitos: bool,
}

fn build_process_tree(sys: &System, filter: &str) -> Vec<ProcessTreeNode> {
    let procs: Vec<_> = sys.processes().iter().collect();

    // PID → index map for O(1) parent lookup
    let pid_to_idx: HashMap<u32, usize> = procs
        .iter()
        .enumerate()
        .map(|(i, (pid, _))| (pid.as_u32(), i))
        .collect();

    // First pass: create nodes
    let mut nodes: Vec<ProcessTreeNode> = procs
        .iter()
        .filter(|(_, p)| {
            filter.is_empty() || p.name().to_string_lossy().to_lowercase().contains(&filter.to_lowercase())
        })
        .map(|(pid, p)| ProcessTreeNode {
            pid: pid.as_u32(),
            name: p.name().to_string_lossy().to_string(),
            cpu: p.cpu_usage(),
            mem: p.memory(),
            children: Vec::new(),
            parent: None,
            is_mitos: p.name().to_string_lossy().to_lowercase().contains("mitos"),
        })
        .collect();

    // If filter is active, we need to rebuild the pid→idx mapping for this filtered subset
    let filtered_pids: HashSet<u32> = nodes.iter().map(|n| n.pid).collect();
    let filtered_pid_to_idx: HashMap<u32, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.pid, i))
        .collect();

    // Second pass: wire up parent-child relationships (only within filtered set)
    for node in nodes.iter_mut() {
        // Look up original process to find parent
        if let Some(proc) = sys.process(Pid::from_u32(node.pid)) {
            if let Some(parent_pid) = proc.parent().map(|p| p.as_u32()) {
                if filtered_pids.contains(&parent_pid) {
                    if let Some(&parent_idx) = filtered_pid_to_idx.get(&parent_pid) {
                        node.parent = Some(parent_idx);
                    }
                }
            }
        }
    }

    // Third pass: populate children lists
    let parent_refs: Vec<Option<usize>> = nodes.iter().map(|n| n.parent).collect();
    for (child_idx, parent_opt) in parent_refs.iter().enumerate() {
        if let Some(parent_idx) = parent_opt {
            nodes[*parent_idx].children.push(child_idx);
        }
    }

    nodes
}

/// Recursive Reingold–Tilford-lite layout: leaves get assigned left-to-right,
/// internal nodes are centered over their children.
fn layout_tree(nodes: &[ProcessTreeNode], roots: &[usize]) -> Vec<egui::Pos2> {
    let mut positions = vec![egui::Pos2::ZERO; nodes.len()];
    let row_height = 100.0;
    let leaf_spacing = 140.0;
    let mut cursor_x = 0.0;

    fn layout_subtree(
        idx: usize,
        depth: usize,
        nodes: &[ProcessTreeNode],
        positions: &mut [egui::Pos2],
        cursor_x: &mut f32,
        row_height: f32,
        leaf_spacing: f32,
    ) {
        let children: Vec<usize> = nodes[idx].children.clone();
        if children.is_empty() {
            positions[idx] = egui::pos2(*cursor_x, depth as f32 * row_height);
            *cursor_x += leaf_spacing;
        } else {
            for child in children {
                layout_subtree(
                    child,
                    depth + 1,
                    nodes,
                    positions,
                    cursor_x,
                    row_height,
                    leaf_spacing,
                );
            }
            let first = nodes[idx].children[0];
            let last = *nodes[idx].children.last().unwrap();
            let center_x = (positions[first].x + positions[last].x) / 2.0;
            positions[idx] = egui::pos2(center_x, depth as f32 * row_height);
        }
    }

    for &root in roots {
        layout_subtree(
            root,
            0,
            nodes,
            &mut positions,
            &mut cursor_x,
            row_height,
            leaf_spacing,
        );
    }

    positions
}

// ────────────────────────── The egui app ──────────────────────────

struct MitosMonitorApp {
    sys: System,
    cpu_history: VecDeque<f64>,
    ram_history: VecDeque<f64>,
    last_stats: Instant,

    selected_tab: Tab,
    process_filter: String,
    search_query: String,
    target_terminal: Option<u32>,

    // Process tree state
    tree_filter: String,
    selected_process: Option<u32>,

    state: Arc<Mutex<SharedState>>,
    action_tx: mpsc::Sender<MonitorAction>,
}

impl MitosMonitorApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let mut sys = System::new_all();
        sys.refresh_all();

        let state = Arc::new(Mutex::new(SharedState {
            terminals: Vec::new(),
            last_error: None,
            last_info: None,
        }));
        let (action_tx, action_rx) = mpsc::channel::<MonitorAction>(64);
        spawn_ipc_worker(Arc::clone(&state), action_rx);

        Self {
            sys,
            cpu_history: VecDeque::from(vec![0.0; 120]),
            ram_history: VecDeque::from(vec![0.0; 120]),
            last_stats: Instant::now() - Duration::from_secs(1),
            selected_tab: Tab::Overview,
            process_filter: String::new(),
            search_query: String::new(),
            target_terminal: None,
            tree_filter: String::new(),
            selected_process: None,
            state,
            action_tx,
        }
    }

    fn maybe_refresh_stats(&mut self) {
        if self.last_stats.elapsed() >= Duration::from_millis(1000) {
            self.last_stats = Instant::now();
            self.sys.refresh_cpu_usage();
            self.sys.refresh_memory();
            self.sys.refresh_processes(sysinfo::ProcessesToUpdate::All);

            self.cpu_history.pop_front();
            self.cpu_history.push_back(self.sys.global_cpu_usage() as f64);

            let ram = if self.sys.total_memory() > 0 {
                self.sys.used_memory() as f64 / self.sys.total_memory() as f64 * 100.0
            } else {
                0.0
            };
            self.ram_history.pop_front();
            self.ram_history.push_back(ram);
        }
    }

    fn current_target(&self) -> Option<u32> {
        self.target_terminal
            .or_else(|| self.state.lock().unwrap().terminals.first().map(|t| t.pid))
    }

    fn send_alert_to_terminal(&self, target_pid: u32, proc_pid: u32, name: &str, cpu: f32) {
        let gauge = RichWidget::Progress {
            percent: (cpu / 100.0).clamp(0.0, 1.0),
            color: Some("red".into()),
        };
        let button = RichWidget::Button {
            label: format!("⚠️ Kill runaway: {} (PID {}) — {:.0}% CPU", name, proc_pid, cpu),
            cmd: format!("kill -9 {}", proc_pid),
        };

        let _ = self.action_tx.try_send(MonitorAction::Inject {
            terminal_pid: target_pid,
            widget: gauge,
        });
        let _ = self.action_tx.try_send(MonitorAction::Inject {
            terminal_pid: target_pid,
            widget: button,
        });
    }

    fn send_alert(&self, pid: u32, name: &str, cpu: f32) {
        let Some(target) = self.current_target() else {
            let mut g = self.state.lock().unwrap();
            g.last_error = Some("No mitos-terminal detected — launch one first".into());
            return;
        };
        self.send_alert_to_terminal(target, pid, name, cpu);
    }
}

impl eframe::App for MitosMonitorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.maybe_refresh_stats();

        // Sidebar for selected process (shown only in ProcessTree tab)
        if self.selected_tab == Tab::ProcessTree {
            if let Some(pid) = self.selected_process {
                egui::SidePanel::right("selected_panel")
                    .resizable(true)
                    .default_width(300.0)
                    .show(ctx, |ui| {
                        self.draw_selected_process_sidebar(ui, pid);
                    });
            }
        }

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.selected_tab, Tab::Overview, "📊 Overview");
                ui.selectable_value(&mut self.selected_tab, Tab::Processes, "⚙️ Processes");
                ui.selectable_value(&mut self.selected_tab, Tab::ProcessTree, "🌳 Tree");
                ui.selectable_value(&mut self.selected_tab, Tab::TerminalBuffers, "📟 Buffers");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.strong("MITOS System Monitor");
                });
            });
        });

        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let g = self.state.lock().unwrap();
                ui.label(format!("🖥️ {} mitos-terminal instance(s) live", g.terminals.len()));
                ui.separator();
                if let Some(e) = &g.last_error {
                    ui.colored_label(egui::Color32::from_rgb(255, 85, 85), e);
                } else if let Some(i) = &g.last_info {
                    ui.colored_label(egui::Color32::from_rgb(85, 255, 85), i);
                } else {
                    ui.label("All systems nominal");
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.selected_tab {
            Tab::Overview => self.draw_overview(ui),
            Tab::Processes => self.draw_processes(ui),
            Tab::ProcessTree => self.draw_process_tree(ui),
            Tab::TerminalBuffers => self.draw_terminal_buffers(ui),
        });

        ctx.request_repaint();
    }
}

impl MitosMonitorApp {
    fn draw_overview(&mut self, ui: &mut egui::Ui) {
        ui.heading(format!(
            "🖥️ {} — {} cores — {:.1} GB RAM",
            System::host_name().unwrap_or_else(|| "mitos".into()),
            self.sys.cpus().len(),
            self.sys.total_memory() as f64 / 1024.0 / 1024.0 / 1024.0
        ));
        ui.add_space(8.0);

        ui.columns(2, |cols| {
            cols[0].group(|ui| {
                ui.heading("CPU %");
                let pts: Vec<[f64; 2]> = self
                    .cpu_history
                    .iter()
                    .enumerate()
                    .map(|(i, &v)| [i as f64, v])
                    .collect();
                Plot::new("cpu_plot")
                    .height(200.0)
                    .include_y(0.0)
                    .include_y(100.0)
                    .show(ui, |plot_ui| {
                        plot_ui.line(
                            Line::new(pts)
                                .name("CPU")
                                .color(egui::Color32::from_rgb(85, 255, 85))
                                .width(2.0),
                        );
                    });
            });
            cols[1].group(|ui| {
                ui.heading("Memory %");
                let pts: Vec<[f64; 2]> = self
                    .ram_history
                    .iter()
                    .enumerate()
                    .map(|(i, &v)| [i as f64, v])
                    .collect();
                Plot::new("ram_plot")
                    .height(200.0)
                    .include_y(0.0)
                    .include_y(100.0)
                    .show(ui, |plot_ui| {
                        plot_ui.line(
                            Line::new(pts)
                                .name("RAM")
                                .color(egui::Color32::from_rgb(85, 85, 255))
                                .width(2.0),
                        );
                    });
            });
        });
    }

    fn draw_processes(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("⚙️ Processes");
            ui.separator();
            ui.text_edit_singleline(&mut self.process_filter);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = match self.current_target() {
                    Some(pid) => format!("🎯 Alerts ➜ terminal PID {pid}"),
                    None => "🎯 Alerts ➜ (no terminal)".into(),
                };
                ui.label(label);
            });
        });

        let mut procs: Vec<(u32, String, f32, u64)> = self
            .sys
            .processes()
            .iter()
            .map(|(pid, p)| {
                (
                    pid.as_u32(),
                    p.name().to_string_lossy().to_string(),
                    p.cpu_usage(),
                    p.memory(),
                )
            })
            .filter(|(_, name, _, _)| {
                self.process_filter.is_empty()
                    || name.to_lowercase().contains(&self.process_filter.to_lowercase())
            })
            .collect();
        procs.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        procs.truncate(200);

        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("proc_grid")
                .num_columns(5)
                .striped(true)
                .min_col_width(70.0)
                .show(ui, |ui| {
                    ui.strong("PID");
                    ui.strong("Name");
                    ui.strong("CPU %");
                    ui.strong("RAM MB");
                    ui.strong("Actions");
                    ui.end_row();

                    for (pid, name, cpu, mem) in procs {
                        ui.label(pid.to_string());
                        ui.label(&name);
                        ui.colored_label(cpu_color(cpu), format!("{:.1}", cpu));
                        ui.label(format!("{:.1}", mem as f64 / 1024.0 / 1024.0));

                        ui.horizontal(|ui| {
                            if ui.button("⚡ Kill").clicked() {
                                if let Some(p) = self.sys.process(Pid::from_u32(pid)) {
                                    let _ = p.kill();
                                }
                            }
                            if ui.button("🚨 Alert ➜ Terminal").clicked() {
                                self.send_alert(pid, &name, cpu);
                            }
                        });
                        ui.end_row();
                    }
                });
        });
    }

    // ────────────────────────── THE PROCESS TREE ──────────────────────────

    fn draw_process_tree(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("🌳 Process Tree");
            ui.separator();
            ui.text_edit_singleline(&mut self.tree_filter);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label("🟢 MITOS   🔴 High CPU   🟡 Med CPU   ⚪ Normal");
            });
        });
        ui.add_space(4.0);

        let nodes = build_process_tree(&self.sys, &self.tree_filter);
        let roots: Vec<usize> = nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.parent.is_none())
            .map(|(i, _)| i)
            .collect();

        if nodes.is_empty() {
            ui.label("No processes match the filter.");
            return;
        }

        let positions = layout_tree(&nodes, &roots);

        // Compute canvas bounds
        let mut max_x = 0.0_f32;
        let mut max_y = 0.0_f32;
        for p in &positions {
            max_x = max_x.max(p.x);
            max_y = max_y.max(p.y);
        }

        let canvas_size = egui::vec2((max_x + 200.0).max(800.0), (max_y + 200.0).max(600.0));
        let node_radius = 18.0;
        let font_body = egui::TextStyle::Body.resolve(ui.style());
        let font_small = egui::TextStyle::Small.resolve(ui.style());

        egui::ScrollArea::both()
            .id_salt("tree_scroll")
            .show(ui, |ui| {
                let (response, painter) = ui.allocate_painter(canvas_size, egui::Sense::click());

                // Draw edges first (orthogonal routing: down → across → down)
                for (i, node) in nodes.iter().enumerate() {
                    for &child in &node.children {
                        let from = positions[i];
                        let to = positions[child];
                        let mid_y = (from.y + to.y) / 2.0;
                        let mid1 = egui::pos2(from.x, mid_y);
                        let mid2 = egui::pos2(to.x, mid_y);

                        let stroke = if nodes[child].is_mitos || node.is_mitos {
                            egui::Stroke::new(1.5, egui::Color32::from_rgb(85, 255, 85))
                        } else {
                            egui::Stroke::new(1.0, egui::Color32::from_gray(70))
                        };
                        painter.line_segment([from, mid1], stroke);
                        painter.line_segment([mid1, mid2], stroke);
                        painter.line_segment([mid2, to], stroke);
                    }
                }

                // Draw nodes
                for (i, node) in nodes.iter().enumerate() {
                    let pos = positions[i];
                    let rect = egui::Rect::from_center_size(
                        pos,
                        egui::vec2(node_radius * 2.0 + 4.0, node_radius * 2.0 + 4.0),
                    );
                    let node_response =
                        ui.interact(rect, egui::Id::new(("node", i)), egui::Sense::click());

                    let base_color = if node.is_mitos {
                        egui::Color32::from_rgb(85, 255, 85)
                    } else if node.cpu > 50.0 {
                        egui::Color32::from_rgb(255, 85, 85)
                    } else if node.cpu > 10.0 {
                        egui::Color32::from_rgb(255, 255, 85)
                    } else {
                        egui::Color32::from_gray(110)
                    };

                    let is_selected = self.selected_process == Some(node.pid);
                    let stroke_color = if is_selected {
                        egui::Color32::WHITE
                    } else if node_response.hovered() {
                        egui::Color32::from_gray(230)
                    } else {
                        egui::Color32::TRANSPARENT
                    };

                    painter.circle_filled(pos, node_radius, base_color);
                    if stroke_color != egui::Color32::TRANSPARENT {
                        painter.circle_stroke(
                            pos,
                            node_radius + 1.0,
                            egui::Stroke::new(2.5, stroke_color),
                        );
                    }

                    // Name label below node
                    painter.text(
                        pos + egui::vec2(0.0, node_radius + 4.0),
                        egui::Align2::CENTER_TOP,
                        &node.name,
                        font_body.clone(),
                        egui::Color32::WHITE,
                    );
                    // PID sub-label
                    painter.text(
                        pos + egui::vec2(0.0, node_radius + 22.0),
                        egui::Align2::CENTER_TOP,
                        format!("PID {} • {:.0}%", node.pid, node.cpu),
                        font_small.clone(),
                        egui::Color32::from_gray(170),
                    );

                    if node_response.clicked() {
                        self.selected_process = Some(node.pid);
                    }
                }
            });
    }

    fn draw_selected_process_sidebar(&mut self, ui: &mut egui::Ui, pid: u32) {
        ui.heading("🔍 Process Details");
        ui.separator();

        let Some(proc) = self.sys.process(Pid::from_u32(pid)) else {
            ui.label("Process no longer exists.");
            if ui.button("✕ Deselect").clicked() {
                self.selected_process = None;
            }
            return;
        };

        let name = proc.name().to_string_lossy().to_string();
        let cpu = proc.cpu_usage();
        let mem = proc.memory();
        let parent_pid = proc.parent().map(|p| p.as_u32());
        let cmd = proc.cmd().join(" ");
        let exe = proc
            .exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "—".into());

        egui::Grid::new("details_grid")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.strong("Name:");
                ui.label(&name);
                ui.end_row();

                ui.strong("PID:");
                ui.monospace(pid.to_string());
                ui.end_row();

                ui.strong("Parent:");
                ui.monospace(parent_pid.map(|p| p.to_string()).unwrap_or_else(|| "—".into()));
                ui.end_row();

                ui.strong("CPU:");
                ui.colored_label(cpu_color(cpu), format!("{:.1}%", cpu));
                ui.end_row();

                ui.strong("Memory:");
                ui.label(format!("{:.1} MB", mem as f64 / 1024.0 / 1024.0));
                ui.end_row();

                ui.strong("Exe:");
                ui.label(egui::RichText::new(&exe).small());
                ui.end_row();
            });

        ui.add_space(8.0);
        ui.label(egui::RichText::new("Command:").strong());
        egui::ScrollArea::vertical().max_height(80.0).show(ui, |ui| {
            ui.monospace(&cmd);
        });

        ui.add_space(16.0);
        ui.heading("⚡ Actions");

        if ui
            .button(egui::RichText::new("💀 Kill Process").strong())
            .clicked()
        {
            let _ = proc.kill();
            self.selected_process = None;
        }

        ui.add_space(8.0);

        let target_label = match self.current_target() {
            Some(p) => format!("🚨 Alert ➜ terminal PID {}", p),
            None => "🚨 Alert ➜ (no terminal)".into(),
        };
        if ui.button(egui::RichText::new(&target_label).strong()).clicked() {
            self.send_alert(pid, &name, cpu);
        }

        ui.add_space(16.0);
        if ui.button("✕ Deselect").clicked() {
            self.selected_process = None;
        }
    }

    fn draw_terminal_buffers(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("📟 Live Terminal Buffers");
            if ui.button("🔄 Refresh").clicked() {
                let _ = self.action_tx.try_send(MonitorAction::Refresh);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.search_query)
                        .desired_width(250.0)
                        .hint_text("🔍 Search ALL terminal history…"),
                );
            });
        });
        ui.add_space(4.0);

        let g = self.state.lock().unwrap();

        if g.terminals.is_empty() {
            ui.label("No mitos-terminal instances detected.");
            ui.label("Launch mitos-terminal — it will appear here automatically within 1 second.");
            return;
        }

        for snap in &g.terminals {
            let alive = snap.last_seen.elapsed() < Duration::from_secs(3);

            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.radio_value(
                        &mut self.target_terminal,
                        Some(snap.pid),
                        format!("PID {} — {}", snap.pid, snap.prompt.trim()),
                    );
                    ui.label(if alive { "🟢 live" } else { "🔴 stale" });
                    if self.target_terminal == Some(snap.pid) {
                        ui.colored_label(egui::Color32::from_rgb(85, 255, 85), "🎯 alert target");
                    }
                });

                if !self.search_query.trim().is_empty() {
                    let q = self.search_query.as_str();
                    let matches: Vec<(usize, &str)> = snap
                        .text
                        .lines()
                        .enumerate()
                        .filter(|(_, l)| l.contains(q))
                        .collect();

                    ui.label(format!("🔎 {} match(es)", matches.len()));
                    for (line_no, line) in matches.iter().take(50) {
                        ui.horizontal(|ui| {
                            ui.monospace(format!("L{:04}", line_no + 1));
                            ui.colored_label(egui::Color32::from_rgb(255, 255, 85), line.trim_end());
                        });
                    }
                }

                egui::CollapsingHeader::new(format!(
                    "📄 View full buffer ({} lines)",
                    snap.text.lines().count()
                ))
                .id_salt(format!("buf_{}", snap.pid))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                        let mut text = snap.text.as_str();
                        ui.add(
                            egui::TextEdit::multiline(&mut text)
                                .font(egui::TextStyle::Monospace)
                                .desired_width(f32::INFINITY)
                                .text_color(egui::Color32::from_gray(200)),
                        );
                    });
                });
            });
        }
    }
}

fn cpu_color(cpu: f32) -> egui::Color32 {
    if cpu > 85.0 {
        egui::Color32::from_rgb(255, 85, 85)
    } else if cpu > 50.0 {
        egui::Color32::from_rgb(255, 255, 85)
    } else {
        egui::Color32::from_rgb(85, 255, 85)
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_title("MITOS System Monitor"),
        ..Default::default()
    };
    eframe::run_native(
        "mitos-system-monitor",
        options,
        Box::new(|cc| Ok(Box::new(MitosMonitorApp::new(cc)))),
    )
}
