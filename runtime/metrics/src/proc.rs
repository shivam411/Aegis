//! Per-process counters from `/proc` (Linux). Used for apps without a
//! cgroup, and for the thread and file-descriptor counts of every app.

use std::path::Path;

/// Totals over a set of processes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProcTotals {
    /// User + system CPU time, in microseconds.
    pub cpu_usec: u64,
    /// Resident memory.
    pub rss_bytes: u64,
    pub threads: u64,
    pub fds: u64,
    pub procs: u64,
}

struct Stat {
    pgrp: u32,
    cpu_ticks: u64,
    threads: u64,
    rss_pages: u64,
}

/// Parses /proc/<pid>/stat. The command name (field 2) can contain spaces
/// and parentheses, so fields are counted from the last ')'.
fn parse_stat(text: &str) -> Option<Stat> {
    let rest = &text[text.rfind(')')? + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    // f[0] is field 3 (state); field N is f[N - 3].
    Some(Stat {
        pgrp: f.get(2)?.parse().ok()?,
        cpu_ticks: f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?,
        threads: f.get(17)?.parse().ok()?,
        rss_pages: f.get(21)?.parse().ok()?,
    })
}

fn read_stat(pid: u32) -> Option<Stat> {
    parse_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

fn clock_ticks() -> u64 {
    #[cfg(unix)]
    {
        let t = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if t > 0 {
            return t as u64;
        }
    }
    100
}

fn page_size() -> u64 {
    #[cfg(unix)]
    {
        let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if p > 0 {
            return p as u64;
        }
    }
    4096
}

fn count_fds(pid: u32) -> u64 {
    std::fs::read_dir(format!("/proc/{pid}/fd"))
        .map(|d| d.count() as u64)
        .unwrap_or(0)
}

/// Totals for the given processes (e.g. the members of a cgroup).
pub fn totals(pids: &[u32]) -> ProcTotals {
    let (ticks, page) = (clock_ticks(), page_size());
    let mut t = ProcTotals::default();
    for &pid in pids {
        if let Some(stat) = read_stat(pid) {
            t.cpu_usec += stat.cpu_ticks * 1_000_000 / ticks;
            t.rss_bytes += stat.rss_pages * page;
            t.threads += stat.threads;
            t.fds += count_fds(pid);
            t.procs += 1;
        }
    }
    t
}

/// Members of a process group. The supervisor starts every app as the
/// leader of its own group, so this is the app and everything it forked
/// (unless something called setsid/setpgid).
pub fn process_group(pgid: u32) -> Vec<u32> {
    if !Path::new("/proc").exists() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| read_stat(*pid).is_some_and(|s| s.pgrp == pgid))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_stat_with_awkward_names() {
        let line = "1234 (my (odd) app) S 1 1234 1234 0 -1 4194560 100 0 0 0 250 50 0 0 20 0 7 0 12345 1000000 300 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0";
        let s = parse_stat(line).unwrap();
        assert_eq!(s.pgrp, 1234);
        assert_eq!(s.cpu_ticks, 300);
        assert_eq!(s.threads, 7);
        assert_eq!(s.rss_pages, 300);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_totals_for_this_process() {
        let me = std::process::id();
        let t = totals(&[me]);
        assert_eq!(t.procs, 1);
        assert!(t.rss_bytes > 0);
        assert!(t.threads >= 1);
        assert!(t.fds >= 3);
        assert_eq!(totals(&[u32::MAX]).procs, 0);
    }
}
