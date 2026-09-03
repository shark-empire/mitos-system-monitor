use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use sysinfo::System;
use std::collections::VecDeque;

struct MitosMonitorApp {
    sys: System,
    
    // Rolling history for graphs (keeps the last 100 data points)
    cpu_history: VecDeque<f64>,
    ram_history: VecDeque<f64>,
    
    // UI State
    selected_tab: MonitorTab,
    search_query: String,
}

#[derive(PartialEq)]
enum MonitorTab {
    Overview,
    Processes,
    TerminalBuffers,
}

impl MitosMonitorApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let mut sys = System::new_all();
        sys.refresh_all();

        Self {
            sys,
            cpu_history: VecDeque::from(vec![0.0; 100]),
            ram_history: VecDeque::from(vec![0.0; 100]),
            selected_tab: MonitorTab::Overview,
            search_query: String::new(),
        }
    }

    fn update_system_stats(&mut self) {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        
        // Push new data and pop old data to keep the graph rolling
        let cpu_usage = self.sys.global_cpu_usage() as f64;
        self.cpu_history.pop_front();
        self.cpu_history.push_back(cpu_usage);

        let ram_usage = (self.sys.used_memory() as f64 / self.sys.total_memory() as f64) * 100.0;
        self.ram_history.pop_front();
        self.ram_history.push_back(ram_usage);
    }
}

impl eframe::App for MitosMonitorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Update stats every frame (approx 60fps, but sysinfo throttles internally if needed)
        self.update_system_stats();

        // Top Menu Bar
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.selected_tab, MonitorTab::Overview, "📊 Overview");
                ui.selectable_value(&mut self.selected_tab, MonitorTab::Processes, "⚙️ Processes");
                ui.selectable_value(&mut self.selected_tab, MonitorTab::TerminalBuffers, "📟 Terminal Buffers");
                
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label("MITOS System Monitor");
                });
            });
        });

        // Main Content Area
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.selected_tab {
                MonitorTab::Overview => self.draw_overview(ui),
                MonitorTab::Processes => self.draw_processes(ui),
                MonitorTab::TerminalBuffers => self.draw_terminal_buffers(ui),
            }
        });

        // Force continuous redraws for live graphs
        ctx.request_repaint();
    }
}

impl MitosMonitorApp {
    fn draw_overview(&mut self, ui: &mut egui::Ui) {
        ui.heading("Real-Time Resource Usage");
        
        ui.columns(2, |cols| {
            // CPU Graph
            cols[0].group(|ui| {
                ui.heading("CPU Usage");
                let cpu_points: PlotPoints = self.cpu_history
                    .iter()
                    .enumerate()
                    .map(|(i, &y)| [i as f64, y])
                    .collect();
                
                Plot::new("cpu_plot")
                    .height(200.0)
                    .y_axis_formatter(|mark, _, _| format!("{:.1}%", mark))
                    .show(ui, |plot_ui| {
                        plot_ui.line(Line::new(cpu_points).color(egui::Color32::from_rgb(85, 255, 85)));
                    });
            });

            // RAM Graph
            cols[1].group(|ui| {
                ui.heading("Memory Usage");
                let ram_points: PlotPoints = self.ram_history
                    .iter()
                    .enumerate()
                    .map(|(i, &y)| [i as f64, y])
                    .collect();
                
                Plot::new("ram_plot")
                    .height(200.0)
                    .y_axis_formatter(|mark, _, _| format!("{:.1}%", mark))
                    .show(ui, |plot_ui| {
                        plot_ui.line(Line::new(ram_points).color(egui::Color32::from_rgb(85, 85, 255)));
                    });
            });
        });
    }

    fn draw_processes(&mut self, ui: &mut egui::Ui) {
        ui.heading("Active Processes");
        ui.text_edit_singleline(&mut self.search_query);
        
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("PID");
                ui.label("Name");
                ui.label("CPU %");
                ui.label("RAM (MB)");
                ui.label("Actions");
            });
            ui.separator();

            for (pid, process) in self.sys.processes() {
                let name = process.name().to_string_lossy();
                
                // Simple search filter
                if !self.search_query.is_empty() && !name.contains(&self.search_query) {
                    continue;
                }

                ui.horizontal(|ui| {
                    ui.label(pid.to_string());
                    ui.label(name);
                    ui.label(format!("{:.1}%", process.cpu_usage()));
                    ui.label(format!("{:.1}", process.memory() as f32 / 1024.0 / 1024.0));
                    
                    // Actionable Monitoring: The MITOS touch!
                    if ui.button("⚠️ Inject MROP Alert").clicked() {
                        // TODO: Send IPC message to mitos-terminal to show a warning widget
                    }
                });
            }
        });
    }

    fn draw_terminal_buffers(&mut self, ui: &mut egui::Ui) {
        ui.heading("Global Terminal Search & Scraping");
        ui.label("Connecting to open mitos-terminal instances via Unix Domain Sockets...");
        
        // TODO: Iterate through /tmp/mitos-term-*.sock
        // Send IpcRequest::GetTerminalBuffer
        // Display the text output here so the user can Cmd+F search their ENTIRE OS history
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 700.0])
            .with_title("MITOS System Monitor"),
        ..Default::default()
    };
    eframe::run_native(
        "mitos-system-monitor",
        options,
        Box::new(|cc| Ok(Box::new(MitosMonitorApp::new(cc)))),
    )
}
