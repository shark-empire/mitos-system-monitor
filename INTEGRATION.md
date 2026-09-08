# MITOS System Monitor Integration

This is the interactive, GPU-accelerated System Monitor for MITOS, built with `eframe` and `egui`. It provides real-time hardware statistics, process management, and deep integration with the MITOS shell.

## How it connects to the MITOS Ecosystem

### 1. Hardware & Process Stats (`sysinfo`)
Rather than parsing `/proc` manually, this application uses the `sysinfo` crate to abstract OS-level process, CPU, and RAM statistics. This ensures cross-platform compatibility during development while maintaining high performance for the real-time `egui_plot` sparklines on the Overview tab.

### 2. Terminal Buffer Scraping (`mitos-utils::ipc`)
This monitor acts as a **client** to the `mitos-terminal` IPC layer. 
- Every second, the `spawn_ipc_worker` tokio background thread polls `/tmp/mitos-term-{pid}.sock`.
- It sends `IpcRequest::GetTerminalBuffer` to scrape the current shell history.
- This powers the **"📟 Buffers"** tab, enabling global search across all open terminal sessions and providing raw text data for screen-readers.

### 3. MROP Widget Injection
The monitor can push interactive UI elements directly into the user's shell. When a process spikes in CPU usage, the user can click **"Alert ➜ Terminal"**. The monitor sends a `RichWidget::Button` (containing the `kill -9 <pid>` payload) over the Unix socket. `mitos-terminal` renders this as a clickable, GPU-accelerated button directly inline with the terminal output.

### 4. Process Tree Visualization
The **"🌳 Tree"** tab parses the parent-child relationships of all running processes. Processes containing "mitos" in their name (e.g., `mitos-session`, `mitos-gui`) are highlighted in green, giving the user a real-time visual map of the MITOS boot chain and daemon supervision hierarchy.
