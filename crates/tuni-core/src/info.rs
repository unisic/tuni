//! What is running under the shell, and what it is listening on.
//!
//! Kero asks `ps` and `lsof`. On Linux both of those read `/proc`, and `lsof`
//! is not installed on a minimal system, so this reads `/proc` directly: it is
//! the same information without a process per refresh and without a dependency
//! that may not be there.
//!
//! Everything below the reading is a pure function over the text `/proc`
//! hands out, which is what makes it testable — the tests feed the exact
//! strings a kernel writes.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// One process running under the shell.
#[derive(Clone, Debug, PartialEq)]
pub struct Process {
    pub pid: u32,
    /// What to call it in a list: the executable's file name.
    pub name: String,
    /// The full path, for a tooltip and for the context menu to copy.
    pub executable: String,
    /// Percent of one CPU, averaged over the process's life, which is what
    /// `ps` reports under `%CPU` and therefore what kero shows.
    pub cpu: f64,
    pub memory_kb: u64,
}

/// A TCP port one of those processes is listening on.
#[derive(Clone, Debug, PartialEq)]
pub struct Port {
    pub port: u16,
    pub pid: u32,
    pub process: String,
}

impl Port {
    /// Where a click on the row goes. Whatever the socket is bound to, the
    /// machine it is bound on is this one.
    #[must_use]
    pub fn url(&self) -> String {
        format!("http://localhost:{}", self.port)
    }
}

/// Everything the Info page draws, read in one pass.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// What the shell itself is, for the heading. Empty when the pane has no
    /// shell running.
    pub shell: String,
    pub processes: Vec<Process>,
    pub ports: Vec<Port>,
}

/// One row of `/proc/<pid>/stat`, as far as this file cares.
#[derive(Clone, Debug, PartialEq)]
pub struct Stat {
    pub pid: u32,
    pub ppid: u32,
    /// `R`, `S`, `Z`, and the rest of the single letters `proc(5)` lists.
    pub state: char,
    /// The `comm` field: the executable's name, truncated to 15 bytes by the
    /// kernel, and only a fallback for a process whose `exe` link cannot be
    /// read.
    pub comm: String,
    /// User plus system time, in clock ticks.
    pub ticks: u64,
    /// When it started, in clock ticks since boot.
    pub start_ticks: u64,
    /// Resident set size, in pages.
    pub rss_pages: u64,
}

/// Everything running under `shell_pid`, and the ports it and its children are
/// listening on.
///
/// A shell with nothing running under it costs one `/proc` walk and no socket
/// reads at all, which is the common case: the panel polls every couple of
/// seconds whether or not anything has changed.
#[must_use]
pub fn snapshot(shell_pid: u32) -> Snapshot {
    if shell_pid == 0 {
        return Snapshot::default();
    }
    let stats = read_stats(Path::new("/proc"));
    let uptime = read_uptime(Path::new("/proc/uptime")).unwrap_or_default();
    let descendants = descendants(&stats, shell_pid);

    let processes: Vec<Process> = descendants
        .iter()
        .filter_map(|pid| {
            let stat = stats.get(pid)?;
            // A zombie is a child that has already exited and is waiting to be
            // reaped: no CPU, no memory, and no signal can touch it. The tree
            // walk still goes through one, which is why it is dropped here
            // rather than while reading.
            if stat.state == 'Z' {
                return None;
            }
            let executable = executable(Path::new("/proc"), *pid);
            let name = executable.as_ref().map_or_else(
                || stat.comm.clone(),
                |path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| stat.comm.clone())
                },
            );
            Some(Process {
                pid: *pid,
                name,
                executable: executable
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                cpu: cpu_percent(stat, uptime),
                memory_kb: stat.rss_pages.saturating_mul(page_size() / 1024),
            })
        })
        .collect();

    let mut pids: Vec<u32> = vec![shell_pid];
    pids.extend(processes.iter().map(|process| process.pid));
    let ports = ports(Path::new("/proc"), &pids, &processes, &stats);

    let shell = stats
        .get(&shell_pid)
        .map(|stat| stat.comm.clone())
        .unwrap_or_default();

    Snapshot {
        shell,
        processes,
        ports,
    }
}

/// Sends a process a signal: `SIGTERM`, or `SIGKILL` when nothing else worked.
///
/// A pid this application never saw in a snapshot is not signalled, so a stale
/// row cannot fire at whatever has since been given the number.
pub fn terminate(pid: u32, force: bool) {
    if pid == 0 {
        return;
    }
    let Ok(pid) = i32::try_from(pid) else {
        return;
    };
    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
    // Safe: a pid and a signal number, and the kernel checks both. A pid that
    // has exited fails with ESRCH, which is the same as nothing happening.
    unsafe {
        libc::kill(pid, signal);
    }
}

/// Every `/proc/<pid>/stat` the reader is allowed to see.
fn read_stats(proc: &Path) -> HashMap<u32, Stat> {
    let Ok(entries) = std::fs::read_dir(proc) else {
        return HashMap::new();
    };
    let mut stats = HashMap::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.parse::<u32>().is_err() {
            continue;
        }
        // A process that exits between the listing and the read is normal, so
        // an unreadable entry is skipped rather than reported.
        let Ok(text) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        if let Some(stat) = parse_stat(&text) {
            stats.insert(stat.pid, stat);
        }
    }
    stats
}

/// Parses one `/proc/<pid>/stat`.
///
/// The `comm` field is in parentheses and may hold spaces and parentheses of
/// its own — `(Web Content)` and `((sd-pam))` are both real — so the fields
/// after it are found from the last `)` rather than by splitting the line.
#[must_use]
pub fn parse_stat(text: &str) -> Option<Stat> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let pid = text.get(..open)?.trim().parse().ok()?;
    let comm = text.get(open + 1..close)?.to_owned();

    // Field 3 in proc(5) numbering is the first one after comm, so index 0
    // here is state, and field N is at index N - 3.
    let rest: Vec<&str> = text.get(close + 1..)?.split_whitespace().collect();
    let field = |number: usize| rest.get(number - 3).copied();

    Some(Stat {
        pid,
        state: field(3)?.chars().next()?,
        ppid: field(4)?.parse().ok()?,
        comm,
        ticks: field(14)?.parse::<u64>().ok()? + field(15)?.parse::<u64>().ok()?,
        start_ticks: field(22)?.parse().ok()?,
        rss_pages: field(24)?.parse().ok()?,
    })
}

/// Every descendant of `root`, breadth-first.
///
/// Breadth-first because the commands the user ran are the shell's own
/// children, and the workers those spawned matter less; a depth-first walk
/// would bury the first behind the second.
#[must_use]
pub fn descendants(stats: &HashMap<u32, Stat>, root: u32) -> Vec<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for stat in stats.values() {
        children.entry(stat.ppid).or_default().push(stat.pid);
    }
    for list in children.values_mut() {
        list.sort_unstable();
    }

    let mut out = Vec::new();
    let mut seen = HashSet::from([root]);
    let mut queue: std::collections::VecDeque<u32> =
        children.get(&root).cloned().unwrap_or_default().into();
    while let Some(pid) = queue.pop_front() {
        // A pid cannot be its own ancestor, but a `/proc` read that straddles
        // an exit can report a loop, and a loop here would not end.
        if !seen.insert(pid) {
            continue;
        }
        out.push(pid);
        if let Some(next) = children.get(&pid) {
            queue.extend(next);
        }
    }
    out
}

/// Seconds since boot, which is what a process's start time is measured from.
fn read_uptime(path: &Path) -> Option<f64> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_uptime(&text)
}

#[must_use]
pub fn parse_uptime(text: &str) -> Option<f64> {
    text.split_whitespace().next()?.parse().ok()
}

/// What `ps` calls `%CPU`: the share of one processor this process has used
/// over its whole life, not since the last refresh.
#[must_use]
pub fn cpu_percent(stat: &Stat, uptime: f64) -> f64 {
    let hertz = clock_ticks();
    if hertz == 0.0 {
        return 0.0;
    }
    let elapsed = uptime - stat.start_ticks as f64 / hertz;
    if elapsed <= 0.0 {
        return 0.0;
    }
    (stat.ticks as f64 / hertz / elapsed * 100.0).clamp(0.0, 100_000.0)
}

/// The listening TCP ports held by any of `pids`.
fn ports(
    proc: &Path,
    pids: &[u32],
    processes: &[Process],
    stats: &HashMap<u32, Stat>,
) -> Vec<Port> {
    let mut listening = HashMap::new();
    for family in ["net/tcp", "net/tcp6"] {
        if let Ok(text) = std::fs::read_to_string(proc.join(family)) {
            listening.extend(parse_listening(&text));
        }
    }
    if listening.is_empty() {
        return Vec::new();
    }

    let mut ports = Vec::new();
    let mut seen = HashSet::new();
    for pid in pids {
        for inode in socket_inodes(proc, *pid) {
            let Some(port) = listening.get(&inode) else {
                continue;
            };
            // One socket is listed once for IPv4 and once for IPv6, and a
            // forked server holds the same port in every worker.
            if !seen.insert((*pid, *port)) {
                continue;
            }
            let process = processes
                .iter()
                .find(|process| process.pid == *pid)
                .map(|process| process.name.clone())
                .or_else(|| stats.get(pid).map(|stat| stat.comm.clone()))
                .unwrap_or_else(|| "?".to_owned());
            ports.push(Port {
                port: *port,
                pid: *pid,
                process,
            });
        }
    }
    ports.sort_by_key(|port| (port.port, port.pid));
    ports
}

/// The socket inodes a process holds open.
fn socket_inodes(proc: &Path, pid: u32) -> Vec<u64> {
    let Ok(entries) = std::fs::read_dir(proc.join(pid.to_string()).join("fd")) else {
        // Another user's process, or one that exited mid-walk.
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let target = std::fs::read_link(entry.path()).ok()?;
            socket_inode(&target.to_string_lossy())
        })
        .collect()
}

/// `socket:[12345]`, which is what a socket descriptor points at.
#[must_use]
pub fn socket_inode(target: &str) -> Option<u64> {
    target
        .strip_prefix("socket:[")?
        .strip_suffix(']')?
        .parse()
        .ok()
}

/// Inode to port, for every socket in `LISTEN`.
///
/// The format is `/proc/net/tcp`: a header line, then one row a socket with
/// the local address as `HEXADDR:HEXPORT`, the state in field 3, and the
/// inode in field 9.
#[must_use]
pub fn parse_listening(text: &str) -> HashMap<u64, u16> {
    const LISTEN: &str = "0A";
    let mut out = HashMap::new();
    for line in text.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (Some(local), Some(state), Some(inode)) = (fields.get(1), fields.get(3), fields.get(9))
        else {
            continue;
        };
        if *state != LISTEN {
            continue;
        }
        let Some((_, port)) = local.rsplit_once(':') else {
            continue;
        };
        let (Ok(port), Ok(inode)) = (u16::from_str_radix(port, 16), inode.parse::<u64>()) else {
            continue;
        };
        out.insert(inode, port);
    }
    out
}

/// The full path of what a process is running, when it can be read.
fn executable(proc: &Path, pid: u32) -> Option<PathBuf> {
    std::fs::read_link(proc.join(pid.to_string()).join("exe")).ok()
}

fn clock_ticks() -> f64 {
    // Safe: sysconf takes a name and returns a number.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks > 0 { ticks as f64 } else { 100.0 }
}

fn page_size() -> u64 {
    // Safe: as above.
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if size > 0 { size as u64 } else { 4096 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line in the shape a kernel writes: fields 3 to 24 in order, with 100
    /// ticks of user time, 50 of system, a start 900 ticks after boot, and 512
    /// resident pages. The fields past 24 are there because a real line has
    /// them.
    fn stat_line(pid: u32, ppid: u32, state: char, comm: &str) -> String {
        let tail = ["0"; 20].join(" ");
        format!(
            "{pid} ({comm}) {state} {ppid} 0 0 0 -1 0 0 0 0 0 100 50 0 0 20 0 1 0 900 0 512 {tail}"
        )
    }

    #[test]
    fn a_stat_line_parses_into_the_fields_the_panel_needs() {
        let stat = parse_stat(&stat_line(42, 7, 'S', "zsh")).expect("stat");
        assert_eq!(stat.pid, 42);
        assert_eq!(stat.ppid, 7);
        assert_eq!(stat.state, 'S');
        assert_eq!(stat.comm, "zsh");
        assert_eq!(stat.ticks, 150, "user and system time added together");
        assert_eq!(stat.start_ticks, 900);
        assert_eq!(stat.rss_pages, 512);
    }

    #[test]
    fn a_command_with_spaces_and_parentheses_still_parses() {
        let stat = parse_stat(&stat_line(9, 1, 'R', "Web Content (tab)")).expect("stat");
        assert_eq!(stat.comm, "Web Content (tab)");
        assert_eq!(stat.ppid, 1);
        assert_eq!(stat.state, 'R');
    }

    #[test]
    fn descendants_are_breadth_first_from_the_shell() {
        let stats: HashMap<u32, Stat> = [
            stat_line(10, 1, 'S', "zsh"),
            stat_line(11, 10, 'S', "make"),
            stat_line(12, 10, 'S', "vim"),
            stat_line(13, 11, 'R', "cc"),
            stat_line(20, 1, 'S', "other"),
        ]
        .iter()
        .filter_map(|line| parse_stat(line))
        .map(|stat| (stat.pid, stat))
        .collect();

        assert_eq!(
            descendants(&stats, 10),
            vec![11, 12, 13],
            "the shell's own children before what they spawned"
        );
        assert!(descendants(&stats, 20).is_empty());
    }

    #[test]
    fn a_parent_loop_does_not_hang_the_walk() {
        // Two processes each claiming the other as a parent, which a read that
        // straddles an exit can produce.
        let stats: HashMap<u32, Stat> = [stat_line(2, 3, 'S', "a"), stat_line(3, 2, 'S', "b")]
            .iter()
            .filter_map(|line| parse_stat(line))
            .map(|stat| (stat.pid, stat))
            .collect();
        assert_eq!(descendants(&stats, 2), vec![3]);
    }

    #[test]
    fn listening_sockets_are_read_out_of_proc_net_tcp() {
        let text = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 54321 1 0000 100 0
   1: 0100007F:8080 0100007F:CAFE 01 00000000:00000000 00:00000000 00000000  1000        0 54322 1 0000 100 0
   2: 00000000:0FA0 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 54323 1 0000 100 0
";
        let listening = parse_listening(text);
        assert_eq!(listening.get(&54321), Some(&8080));
        assert_eq!(listening.get(&54323), Some(&4000));
        assert!(
            !listening.contains_key(&54322),
            "an established connection is not a listening port"
        );
    }

    #[test]
    fn a_socket_descriptor_names_its_inode() {
        assert_eq!(socket_inode("socket:[54321]"), Some(54321));
        assert_eq!(socket_inode("/dev/pts/3"), None);
        assert_eq!(socket_inode("anon_inode:[eventpoll]"), None);
    }

    #[test]
    fn cpu_percent_is_the_share_of_one_processor_over_a_life() {
        let mut stat = parse_stat(&stat_line(1, 0, 'R', "busy")).expect("stat");
        // Half a processor: 150 ticks of CPU over 300 ticks of wall clock.
        stat.start_ticks = 0;
        let uptime = 300.0 / clock_ticks();
        assert!((cpu_percent(&stat, uptime) - 50.0).abs() < 0.5);
    }

    #[test]
    fn a_process_that_has_not_started_yet_reports_no_cpu() {
        let mut stat = parse_stat(&stat_line(1, 0, 'R', "new")).expect("stat");
        stat.start_ticks = 1_000_000;
        assert_eq!(cpu_percent(&stat, 1.0), 0.0);
    }

    #[test]
    fn a_port_row_points_at_this_machine() {
        let port = Port {
            port: 3000,
            pid: 5,
            process: "node".to_owned(),
        };
        assert_eq!(port.url(), "http://localhost:3000");
    }
}
