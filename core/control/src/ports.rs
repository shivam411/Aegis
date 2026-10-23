//! Port safety: a release only starts on a port nobody else uses, and a
//! health check only counts when the new release is the one answering.
//!
//! Without this, a release whose port is held by another process (say the
//! old PM2 copy of the app) crashes with `EADDRINUSE` while the health check
//! is answered by the other process, and the broken release is promoted.
//!
//! What is guaranteed:
//!
//! * Before any release process (or container) starts, its `PORT` must be
//!   free: nothing may be bound to it on 127.0.0.1, 0.0.0.0 or [::], and
//!   nothing may accept a connection on 127.0.0.1. Otherwise the deployment
//!   (or start action) fails with [`PortInUse`], naming the other process
//!   when the daemon's user can see it.
//! * After the health check passes, the release must keep running (same
//!   process, no restart) for the liveness window, or the deployment fails
//!   and rolls back.
//! * On Linux, for process apps, the socket listening on the probed port
//!   must belong to the release's process tree (matched by socket inode via
//!   `/proc/net/tcp*` and `/proc/<pid>/fd`). Container apps are reached
//!   through Docker's port publishing, so this check is skipped for them.

use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

/// Another process listens on the port a release was going to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortInUse {
    pub port: u16,
    /// e.g. `pid 1234 (node)`, when visible to the daemon's user.
    pub owner: Option<String>,
}

impl fmt::Display for PortInUse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "port {} is already in use by another process", self.port)?;
        match &self.owner {
            Some(owner) => write!(f, " ({})", owner)?,
            None => write!(
                f,
                " (not visible to the daemon's user; `sudo ss -ltnp 'sport = :{}'` shows it)",
                self.port
            )?,
        }
        write!(
            f,
            "; map a domain so each release gets its own port, or stop the other process (for example the app's old PM2 or systemd copy)"
        )
    }
}

impl std::error::Error for PortInUse {}

fn bind_conflicts(addr: SocketAddr) -> bool {
    match TcpListener::bind(addr) {
        Ok(_) => false,
        Err(e) => e.kind() == std::io::ErrorKind::AddrInUse,
    }
}

/// Whether something already uses `port`: a bind on loopback, all IPv4 or
/// all IPv6 addresses conflicts, or something accepts connections on
/// 127.0.0.1.
pub fn port_in_use(port: u16) -> bool {
    bind_conflicts(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
        || bind_conflicts(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))
        || bind_conflicts(SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)))
        || TcpStream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            Duration::from_millis(300),
        )
        .is_ok()
}

/// Waits up to `grace` for `port` to become free (a process that was just
/// stopped may take a moment to release it).
pub(crate) async fn ensure_port_free(port: u16, grace: Duration) -> Result<(), PortInUse> {
    let deadline = tokio::time::Instant::now() + grace;
    loop {
        let busy = tokio::task::spawn_blocking(move || port_in_use(port))
            .await
            .unwrap_or(true);
        if !busy {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            let owner = tokio::task::spawn_blocking(move || describe_listener(port))
                .await
                .ok()
                .flatten();
            return Err(PortInUse { port, owner });
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

/// Who owns the socket listening on a port, as far as `/proc` shows.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) enum Listener {
    /// The release's own process tree.
    Ours,
    /// Some other process (`pid 12 (node)` when visible).
    Other(Option<String>),
    /// Nothing visible listens there (another network namespace, no
    /// `/proc`, not Linux).
    Unknown,
}

/// Whether the socket listening on `port` belongs to `root_pid` or one of
/// its descendants.
pub(crate) fn listener_of(port: u16, root_pid: u32) -> Listener {
    #[cfg(target_os = "linux")]
    {
        linux::listener_of(port, root_pid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (port, root_pid);
        Listener::Unknown
    }
}

/// `pid N (name)` of the process listening on `port`, if visible.
pub(crate) fn describe_listener(port: u16) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        linux::describe_listener(port)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = port;
        None
    }
}

/// The port a health check URL probes, when it probes this machine.
pub(crate) fn local_probe_port(url: &str) -> Option<u16> {
    let target = url
        .strip_prefix("tcp://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let authority = target.split('/').next()?;
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (host, rest) = rest.split_once(']')?;
        (host, rest.strip_prefix(':')?)
    } else {
        authority.rsplit_once(':')?
    };
    let local = matches!(host, "127.0.0.1" | "localhost" | "::1" | "0.0.0.0" | "");
    local.then(|| port.parse().ok()).flatten()
}

#[cfg(target_os = "linux")]
pub(crate) mod linux {
    use super::Listener;
    use std::collections::{HashMap, HashSet};

    /// Inodes of sockets in LISTEN state on `port` (IPv4 and IPv6).
    pub fn listening_inodes(port: u16) -> HashSet<u64> {
        let mut inodes = HashSet::new();
        for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
            let Ok(text) = std::fs::read_to_string(table) else {
                continue;
            };
            inodes.extend(parse_listening(&text, port));
        }
        inodes
    }

    /// Parses a `/proc/net/tcp`-style table: inodes of LISTEN (0A) sockets
    /// whose local port is `port`.
    pub fn parse_listening(text: &str, port: u16) -> Vec<u64> {
        text.lines()
            .skip(1)
            .filter_map(|line| {
                let fields: Vec<&str> = line.split_whitespace().collect();
                let local = fields.get(1)?;
                let state = fields.get(3)?;
                let inode = fields.get(9)?.parse::<u64>().ok()?;
                let local_port = u16::from_str_radix(local.rsplit_once(':')?.1, 16).ok()?;
                (*state == "0A" && local_port == port && inode != 0).then_some(inode)
            })
            .collect()
    }

    fn socket_inodes_of(pid: u32) -> HashSet<u64> {
        let Ok(entries) = std::fs::read_dir(format!("/proc/{}/fd", pid)) else {
            return HashSet::new();
        };
        entries
            .flatten()
            .filter_map(|e| std::fs::read_link(e.path()).ok())
            .filter_map(|target| {
                let text = target.to_string_lossy().to_string();
                text.strip_prefix("socket:[")?
                    .strip_suffix(']')?
                    .parse::<u64>()
                    .ok()
            })
            .collect()
    }

    fn all_pids() -> Vec<u32> {
        std::fs::read_dir("/proc")
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|e| e.file_name().to_string_lossy().parse::<u32>().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `ppid` from `/proc/<pid>/stat` (the field after the command name,
    /// which may itself contain spaces and parentheses).
    pub fn parse_ppid(stat: &str) -> Option<u32> {
        let after = &stat[stat.rfind(')')? + 1..];
        after.split_whitespace().nth(1)?.parse().ok()
    }

    /// `root` and all its descendants.
    fn process_tree(root: u32) -> HashSet<u32> {
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for pid in all_pids() {
            if let Some(ppid) = std::fs::read_to_string(format!("/proc/{}/stat", pid))
                .ok()
                .and_then(|s| parse_ppid(&s))
            {
                children.entry(ppid).or_default().push(pid);
            }
        }
        let mut tree = HashSet::from([root]);
        let mut queue = vec![root];
        while let Some(pid) = queue.pop() {
            for child in children.get(&pid).into_iter().flatten() {
                if tree.insert(*child) {
                    queue.push(*child);
                }
            }
        }
        tree
    }

    fn name_of(pid: u32) -> String {
        std::fs::read_to_string(format!("/proc/{}/comm", pid))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "?".into())
    }

    fn owner_of(inodes: &HashSet<u64>) -> Option<String> {
        all_pids()
            .into_iter()
            .find(|pid| !socket_inodes_of(*pid).is_disjoint(inodes))
            .map(|pid| format!("pid {} ({})", pid, name_of(pid)))
    }

    pub fn describe_listener(port: u16) -> Option<String> {
        let inodes = listening_inodes(port);
        if inodes.is_empty() {
            return None;
        }
        owner_of(&inodes)
    }

    pub fn listener_of(port: u16, root_pid: u32) -> Listener {
        let inodes = listening_inodes(port);
        if inodes.is_empty() {
            return Listener::Unknown;
        }
        if process_tree(root_pid)
            .into_iter()
            .any(|pid| !socket_inodes_of(pid).is_disjoint(&inodes))
        {
            return Listener::Ours;
        }
        Listener::Other(owner_of(&inodes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_ports_are_detected() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(port_in_use(port));
        drop(listener);
        assert!(!port_in_use(port));
        // Held on all addresses: also busy.
        let any = TcpListener::bind(("0.0.0.0", 0)).unwrap();
        assert!(port_in_use(any.local_addr().unwrap().port()));
    }

    #[test]
    fn error_message_explains_what_to_do() {
        let e = PortInUse {
            port: 4000,
            owner: Some("pid 7 (node)".into()),
        };
        let text = e.to_string();
        assert!(text.starts_with("port 4000 is already in use by another process (pid 7 (node))"));
        assert!(text.contains("map a domain"));
        let hidden = PortInUse {
            port: 4000,
            owner: None,
        }
        .to_string();
        assert!(hidden.contains("ss -ltnp"), "{hidden}");
    }

    #[test]
    fn probe_ports() {
        assert_eq!(
            local_probe_port("http://127.0.0.1:4000/api/health"),
            Some(4000)
        );
        assert_eq!(local_probe_port("http://localhost:3000"), Some(3000));
        assert_eq!(local_probe_port("tcp://127.0.0.1:5432"), Some(5432));
        assert_eq!(local_probe_port("http://[::1]:8080/x"), Some(8080));
        assert_eq!(local_probe_port("http://example.com:80/"), None);
        assert_eq!(local_probe_port("http://127.0.0.1/health"), None);
    }

    #[tokio::test]
    async fn waiting_for_a_port_reports_its_owner() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let err = ensure_port_free(port, Duration::from_millis(200))
            .await
            .unwrap_err();
        assert_eq!(err.port, port);
        #[cfg(target_os = "linux")]
        {
            let me = std::process::id();
            assert_eq!(
                err.owner.as_deref(),
                Some(format!("pid {} ({})", me, linux_name()).as_str())
            );
            assert_eq!(listener_of(port, me), Listener::Ours);
            // A process tree that doesn't include this test process.
            assert_eq!(
                listener_of(port, 999_999_999),
                Listener::Other(err.owner.clone())
            );
        }
        drop(listener);
        ensure_port_free(port, Duration::from_millis(200))
            .await
            .unwrap();
    }

    #[cfg(target_os = "linux")]
    fn linux_name() -> String {
        std::fs::read_to_string("/proc/self/comm")
            .unwrap()
            .trim()
            .to_string()
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_parsing() {
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:0FA0 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4242 1 0000000000000000 100 0 0 10 0
   1: 0100007F:0FA0 0100007F:9C40 01 00000000:00000000 00:00000000 00000000  1000        0 4343 1 0000000000000000 100 0 0 10 0
   2: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4444 1 0000000000000000 100 0 0 10 0
";
        assert_eq!(linux::parse_listening(table, 4000), vec![4242]);
        assert_eq!(linux::parse_listening(table, 8080), vec![4444]);
        assert_eq!(
            linux::parse_ppid("123 (my (odd) app) S 77 123 123 0"),
            Some(77)
        );
    }
}
