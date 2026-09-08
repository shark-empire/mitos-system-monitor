# MITOS System Monitor Integration

This is the core system monitoring daemon for MITOS. It gathers hardware statistics and provides accessibility features by scraping terminal buffers.

## How it connects to the MITOS Ecosystem

### 1. Zero-Dependency `/proc` Reading
Instead of pulling in heavy external crates like `sysinfo`, this daemon reads `/proc/meminfo` and `/proc/stat` directly. This logic is identical to the `ps`, `free`, and `uptime` applets found in `mitos-utils`, ensuring a consistent view of system resources across the entire OS without bloating the binary size.

### 2. Terminal Buffer Scraping (Accessibility & Global Search)
`mitos-terminal` exposes a read-only Unix Domain Socket for every open window at `/tmp/mitos-term-{pid}.sock`. 
This daemon acts as a **client** to those sockets. By connecting to them, it can:
- Read the text content of every open terminal buffer.
- Power "Global Search" features (finding a string across all open tabs).
- Provide raw text data to screen-readers for accessibility compliance.

### 3. MROP Visualizations
When CLI tools like `mitos-top` or `mitos-btop` are run inside `mitos-terminal`, they do not draw ASCII art graphs. Instead, they output `RichWidget::Sparkline` and `RichWidget::Progress` sequences. The terminal renders these as high-performance, GPU-accelerated `egui` charts, while this daemon provides the underlying data stream.

### 4. IPC Architecture
Currently runs as a polling loop. In a future stage, this will expose a Unix socket at `/run/mitos-system-monitor.sock` to serve live stats to `mitos-gui`'s top bar and `mitos-settings`'s hardware info panel.
