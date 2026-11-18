//! events.log in the data dir (Trace in Records.swift): local ISO 8601 time with fractions, one line per event.
//! Past 256 KB the older half is dropped. Also echoed to stderr.
use std::io::{Read, Seek, SeekFrom, Write};

const LIMIT: u64 = 256 << 10;

pub fn log(s: &str) {
    let line = format!(
        "{} {s}\n",
        chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z")
    );
    eprint!("{line}");
    let path = crate::config::data_dir().join("events.log");
    let Ok(mut f) = std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .open(&path)
    else {
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(&path, line);
        return;
    };
    let end = f.metadata().map(|m| m.len()).unwrap_or(0);
    if end <= LIMIT {
        let _ = f.write_all(line.as_bytes());
        return;
    }
    let mut rest = vec![];
    if f.seek(SeekFrom::Start(end / 2))
        .and_then(|_| f.read_to_end(&mut rest))
        .is_err()
    {
        return;
    }
    let start = rest.iter().position(|&b| b == b'\n').map_or(0, |i| i + 1);
    rest.drain(..start);
    rest.extend_from_slice(line.as_bytes());
    let _ = std::fs::write(&path, rest);
}
