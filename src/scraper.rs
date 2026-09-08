// src/scraper.rs
use std::io::{self, Read};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

/// Represents a scraped terminal buffer
pub struct TerminalBuffer {
    pub pid: u32,
    pub socket_path: PathBuf,
    pub content: String,
}

/// Scans /tmp for mitos-term-*.sock sockets, connects to each,
/// and reads the terminal buffer.
pub fn scrape_all_terminals() -> Vec<TerminalBuffer> {
    let mut buffers = Vec::new();
    
    // mitos-terminal exposes sockets as /tmp/mitos-term-{pid}.sock
    let pattern = "/tmp/mitos-term-*.sock";
    for entry in glob::glob(pattern).unwrap() {
        if let Ok(path) = entry {
            if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                if let Some(pid_str) = filename.strip_prefix("mitos-term-").and_then(|s| s.strip_suffix(".sock")) {
                    if let Ok(pid) = pid_str.parse::<u32>() {
                        if let Ok(buffer) = scrape_terminal(&path, pid) {
                            buffers.push(buffer);
                        }
                    }
                }
            }
        }
    }
    buffers
}

fn scrape_terminal(path: &PathBuf, pid: u32) -> io::Result<TerminalBuffer> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    
    // Assuming mitos-terminal sends the buffer as plain text upon connection
    let mut content = String::new();
    stream.read_to_string(&mut content)?;
    
    Ok(TerminalBuffer {
        pid,
        socket_path: path.clone(),
        content,
    })
}
