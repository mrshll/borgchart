//! Process trees, for `brg down`: the processes under the panes it stops,
//! found before the panes are killed and ended by PID afterwards. A child
//! that left its pane's process group or session (with `setsid`, or inside
//! a sandbox's own PID namespace) outlives its pane otherwise.
//!
//! A process is known by its PID and start time, so a PID reused by an
//! unrelated process after the snapshot is never signalled.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process};

/// How long processes have to exit after TERM before they get KILL.
pub const GRACE: Duration = Duration::from_secs(2);

/// One process, as the process table showed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    /// When it started, in the platform's own terms: compared, never parsed.
    pub start: String,
    pub zombie: bool,
}

/// Each root and every process under it, by parentage, leaving out `keep`
/// and PIDs 0 and 1. Roots missing from the table are skipped.
pub fn tree(table: &[Proc], roots: &[u32], keep: &BTreeSet<u32>) -> Vec<Proc> {
    let mut children: BTreeMap<u32, Vec<&Proc>> = BTreeMap::new();
    for p in table {
        if p.ppid != p.pid {
            children.entry(p.ppid).or_default().push(p);
        }
    }
    let by_pid: BTreeMap<u32, &Proc> = table.iter().map(|p| (p.pid, p)).collect();
    let mut seen = BTreeSet::new();
    let mut stack: Vec<&Proc> = roots
        .iter()
        .filter_map(|r| by_pid.get(r).copied())
        .collect();
    let mut out = Vec::new();
    while let Some(p) = stack.pop() {
        if !seen.insert(p.pid) {
            continue;
        }
        if p.pid > 1 && !keep.contains(&p.pid) {
            out.push(p.clone());
        }
        stack.extend(children.get(&p.pid).into_iter().flatten().copied());
    }
    out.sort_by_key(|p| p.pid);
    out
}

/// `pid` and each of its ancestors in `table`.
pub fn ancestry(table: &[Proc], pid: u32) -> BTreeSet<u32> {
    let by_pid: BTreeMap<u32, u32> = table.iter().map(|p| (p.pid, p.ppid)).collect();
    let mut out = BTreeSet::new();
    let mut at = pid;
    while out.insert(at) {
        match by_pid.get(&at) {
            Some(&ppid) if ppid != 0 => at = ppid,
            _ => break,
        }
    }
    out
}

/// The pane PIDs, of `(pane pid, tmux server pid)` pairs, that are the
/// server's own children in `table`. A pane PID is a number in the
/// server's PID namespace; seen from another (brg run inside a sandbox) it
/// may name an unrelated process, which this leaves out.
pub fn owned(table: &[Proc], roots: &[(u32, u32)]) -> Vec<u32> {
    let ppid: BTreeMap<u32, u32> = table.iter().map(|p| (p.pid, p.ppid)).collect();
    roots
        .iter()
        .filter(|(pane, server)| ppid.get(pane) == Some(server))
        .map(|(pane, _)| *pane)
        .collect()
}

/// The processes under the given panes now (`(pane pid, tmux server pid)`
/// pairs), leaving out brg itself and its ancestors: a member may run
/// `brg down` from inside its own pane.
pub fn snapshot(roots: &[(u32, u32)]) -> Result<Vec<Proc>, String> {
    if roots.is_empty() {
        return Ok(Vec::new());
    }
    let table = table()?;
    let keep = ancestry(&table, std::process::id());
    Ok(tree(&table, &owned(&table, roots), &keep))
}

/// End `procs`, and whatever they started since the snapshot: TERM each
/// one that is still the same process, then KILL any still running after
/// [`GRACE`]. Returns those that outlived KILL. `Err` means the process
/// table couldn't be read again, so the rest were left alone.
pub fn end(procs: &[Proc]) -> Result<Vec<u32>, String> {
    if procs.is_empty() {
        return Ok(Vec::new());
    }
    let table = table()?;
    let mut left = grown(&table, procs, &ancestry(&table, std::process::id()));
    for p in &left {
        signal(p, Signal::TERM);
    }
    let deadline = Instant::now() + GRACE;
    while !left.is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        left = still_running(&left)?;
    }
    for p in &left {
        signal(p, Signal::KILL);
    }
    // KILL can't be ignored; give the kernel a moment to finish.
    let deadline = Instant::now() + Duration::from_secs(1);
    while !left.is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        left = still_running(&left)?;
    }
    Ok(left.iter().map(|p| p.pid).collect())
}

/// Those of `procs` still running as the same process in `table`, with
/// every process under them now, leaving out `keep`.
fn grown(table: &[Proc], procs: &[Proc], keep: &BTreeSet<u32>) -> Vec<Proc> {
    let same: Vec<u32> = procs
        .iter()
        .filter(|p| table.iter().any(|t| t.pid == p.pid && t.start == p.start))
        .map(|p| p.pid)
        .collect();
    tree(table, &same, keep)
        .into_iter()
        .filter(|p| !p.zombie)
        .collect()
}

fn signal(p: &Proc, sig: Signal) {
    if let Some(pid) = Pid::from_raw(p.pid as i32) {
        let _ = kill_process(pid, sig);
    }
}

/// Those of `procs` that are still running as the same process.
fn still_running(procs: &[Proc]) -> Result<Vec<Proc>, String> {
    if procs.is_empty() {
        return Ok(Vec::new());
    }
    let now: BTreeMap<u32, Proc> = lookup(procs.iter().map(|p| p.pid))?
        .into_iter()
        .map(|p| (p.pid, p))
        .collect();
    Ok(procs
        .iter()
        .filter(|p| {
            now.get(&p.pid)
                .is_some_and(|n| n.start == p.start && !n.zombie)
        })
        .cloned()
        .collect())
}

// --- the process table ---

/// Every process.
#[cfg(target_os = "linux")]
fn table() -> Result<Vec<Proc>, String> {
    let dir = std::fs::read_dir("/proc").map_err(|e| format!("cannot read /proc: {e}"))?;
    Ok(dir
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(read_stat)
        .collect())
}

/// Those of `pids` that exist now.
#[cfg(target_os = "linux")]
fn lookup(pids: impl Iterator<Item = u32>) -> Result<Vec<Proc>, String> {
    Ok(pids.filter_map(read_stat).collect())
}

#[cfg(target_os = "linux")]
fn read_stat(pid: u32) -> Option<Proc> {
    parse_stat(
        pid,
        &std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?,
    )
}

/// `/proc/<pid>/stat`: `pid (comm) state ppid …`, the start time 22nd.
/// `comm` may hold spaces and parentheses, so fields count from the last `)`.
#[cfg(any(target_os = "linux", test))]
fn parse_stat(pid: u32, text: &str) -> Option<Proc> {
    let rest = &text[text.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    Some(Proc {
        pid,
        ppid: fields.get(1)?.parse().ok()?,
        start: fields.get(19)?.to_string(),
        zombie: *fields.first()? == "Z",
    })
}

#[cfg(not(target_os = "linux"))]
fn table() -> Result<Vec<Proc>, String> {
    let (ok, procs) = ps(&["-A"])?;
    if !ok || procs.is_empty() {
        return Err("ps could not list processes".into());
    }
    Ok(procs)
}

#[cfg(not(target_os = "linux"))]
fn lookup(pids: impl Iterator<Item = u32>) -> Result<Vec<Proc>, String> {
    let list = pids.map(|p| p.to_string()).collect::<Vec<_>>().join(",");
    // ps exits 1 when some of them don't exist; it lists the rest.
    Ok(ps(&["-p", &list])?.1)
}

/// `ps` with one line per process: pid, ppid, state, start time; and
/// whether it exited 0.
#[cfg(not(target_os = "linux"))]
fn ps(select: &[&str]) -> Result<(bool, Vec<Proc>), String> {
    let out = std::process::Command::new("/bin/ps")
        .env_clear()
        .env("LC_ALL", "C")
        .args(select)
        .args(["-o", "pid=", "-o", "ppid=", "-o", "stat=", "-o", "lstart="])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("cannot run ps: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    Ok((
        out.status.success(),
        text.lines().filter_map(parse_ps).collect(),
    ))
}

#[cfg(any(not(target_os = "linux"), test))]
fn parse_ps(line: &str) -> Option<Proc> {
    let mut f = line.split_whitespace();
    let pid = f.next()?.parse().ok()?;
    let ppid = f.next()?.parse().ok()?;
    let stat = f.next()?;
    let start = f.collect::<Vec<_>>().join(" ");
    (!start.is_empty()).then(|| Proc {
        pid,
        ppid,
        start,
        zombie: stat.starts_with('Z'),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, ppid: u32) -> Proc {
        Proc {
            pid,
            ppid,
            start: format!("t{pid}"),
            zombie: false,
        }
    }

    fn pids(procs: &[Proc]) -> Vec<u32> {
        procs.iter().map(|p| p.pid).collect()
    }

    // Catches: a child that detached from its pane's session but kept its
    // parent being missed, and siblings of the pane being swept in.
    #[test]
    fn the_tree_is_every_level_under_the_roots_only() {
        let table = [
            p(1, 0),
            p(10, 1),
            p(11, 10),
            p(12, 11),
            p(13, 12),
            p(20, 1),
            p(21, 20),
        ];
        assert_eq!(
            pids(&tree(&table, &[10], &BTreeSet::new())),
            [10, 11, 12, 13]
        );
        assert_eq!(
            pids(&tree(&table, &[10, 20], &BTreeSet::new())),
            [10, 11, 12, 13, 20, 21]
        );
        // A root that has already exited contributes nothing.
        assert!(tree(&table, &[99], &BTreeSet::new()).is_empty());
    }

    // Catches: a hang on a table where a process is listed as its own
    // parent, or two point at each other (PID reuse while reading).
    #[test]
    fn cycles_in_the_table_end() {
        let table = [p(10, 1), p(11, 11), p(12, 13), p(13, 12), p(14, 10)];
        assert_eq!(
            pids(&tree(&table, &[10, 12], &BTreeSet::new())),
            [10, 12, 13, 14]
        );
        assert_eq!(ancestry(&table, 12), BTreeSet::from([12, 13]));
    }

    // Catches: `brg down`, run by a member inside its own pane, signalling
    // itself or the shell and agent it runs under.
    #[test]
    fn brg_and_its_ancestors_are_kept() {
        // pane 10 -> agent 11 -> shell 12 -> brg 13; agent 11 -> child 14.
        let table = [
            p(1, 0),
            p(10, 1),
            p(11, 10),
            p(12, 11),
            p(13, 12),
            p(14, 11),
        ];
        let keep = ancestry(&table, 13);
        assert_eq!(keep, BTreeSet::from([1, 10, 11, 12, 13]));
        assert_eq!(pids(&tree(&table, &[10], &keep)), [14]);
    }

    // Catches: signalling an unrelated process when the pane PID tmux
    // reports means something else here (brg in another PID namespace).
    #[test]
    fn only_the_servers_own_children_are_panes() {
        let table = [
            p(1, 0),
            p(500, 1),
            p(10, 500),
            p(11, 10),
            p(20, 1),
            p(21, 20),
        ];
        let roots = owned(&table, &[(10, 500), (20, 500), (99, 500)]);
        assert_eq!(roots, [10]);
        assert_eq!(pids(&tree(&table, &roots, &BTreeSet::new())), [10, 11]);
    }

    // Catches: a child forked after the snapshot surviving, and a PID
    // reused since the snapshot (another start time) being signalled.
    #[test]
    fn ending_follows_new_children_but_not_reused_pids() {
        let snap = [p(10, 1), p(11, 10)];
        let mut now = vec![p(10, 1), p(11, 10), p(12, 11), p(30, 1)];
        assert_eq!(pids(&grown(&now, &snap, &BTreeSet::new())), [10, 11, 12]);
        now[0].start = "reused".into();
        now[1].zombie = true;
        assert_eq!(pids(&grown(&now, &snap, &BTreeSet::new())), [12]);
    }

    // Catches: misreading ppid or start time when the command name holds
    // spaces or parentheses.
    #[test]
    fn stat_fields_count_from_the_last_parenthesis() {
        let line = "4242 (a) b (c) S 17 4242 4242 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 987654 1000 10 1 1 1 1 1";
        let proc = parse_stat(4242, line).unwrap();
        assert_eq!(
            (proc.ppid, proc.start.as_str(), proc.zombie),
            (17, "987654", false)
        );
        let zombie = "7 (z) Z 1 7 7 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 55 0 0";
        assert!(parse_stat(7, zombie).unwrap().zombie);
        let ps = parse_ps("  501   1 Ss   Thu Nov  6 19:59:14 2026").unwrap();
        assert_eq!(
            (ps.pid, ps.ppid, ps.start.as_str()),
            (501, 1, "Thu Nov 6 19:59:14 2026")
        );
        assert!(parse_ps(" 9 1 Z+ Thu Nov  6 19:59:14 2026").unwrap().zombie);
    }
}
