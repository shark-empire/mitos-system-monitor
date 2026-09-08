// src/procfs.rs
use std::fs;
use std::io::{self, BufRead, BufReader};

pub struct MemInfo {
    pub total_kb: u64,
    pub free_kb: u64,
    pub available_kb: u64,
}

pub fn read_meminfo() -> io::Result<MemInfo> {
    let file = fs::File::open("/proc/meminfo")?;
    let reader = BufReader::new(file);
    let mut info = MemInfo { total_kb: 0, free_kb: 0, available_kb: 0 };

    for line in reader.lines() {
        let line = line?;
        if line.starts_with("MemTotal:") {
            info.total_kb = parse_kb(&line);
        } else if line.starts_with("MemFree:") {
            info.free_kb = parse_kb(&line);
        } else if line.starts_with("MemAvailable:") {
            info.available_kb = parse_kb(&line);
        }
    }
    Ok(info)
}

pub struct CpuTimes {
    pub user: u64,
    pub nice: u64,
    pub system: u64,
    pub idle: u64,
}

pub fn read_cpu_times() -> io::Result<CpuTimes> {
    let file = fs::File::open("/proc/stat")?;
    let mut reader = BufReader::new(file);
    let mut first_line = String::new();
    reader.read_line(&mut first_line)?;
    
    // "cpu  12345 678 901 23456 78 9 0 0 0 0"
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 5 { 
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid /proc/stat")); 
    }
    
    Ok(CpuTimes {
        user: parts[1].parse().unwrap_or(0),
        nice: parts[2].parse().unwrap_or(0),
        system: parts[3].parse().unwrap_or(0),
        idle: parts[4].parse().unwrap_or(0),
    })
}

fn parse_kb(line: &str) -> u64 {
    line.split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}
