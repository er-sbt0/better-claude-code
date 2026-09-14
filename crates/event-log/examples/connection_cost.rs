//! What a run's *second* connection to the log costs, measured rather than
//! assumed.
//!
//!     cargo run --release -p event-log --example connection_cost -- <dir>
//!
//! §14's single-writer rule is per connection: a supervised run writes through
//! its own [`EventLog`], never the UI's, so every live run adds one connection
//! for as long as it is held open. `driver::registry` says so in prose and
//! nothing had ever priced it — which matters only because the number of live
//! runs is the number of conversations on the canvas.
//!
//! Reports RSS and open file descriptors as connections accumulate, each one
//! having actually read and written, so the per-connection page cache and the
//! WAL handles are in the number rather than outside it. Linux only: it reads
//! `/proc/self`.

use std::path::PathBuf;

use agent_protocol::{Event, RawEvent, RunId, TurnStarted, SCHEMA_VERSION};
use event_log::{EventLog, LaunchFlags, Run, RunStatus};

const CONNECTIONS: usize = 16;

fn main() {
    let dir = match std::env::args().nth(1) {
        Some(d) => PathBuf::from(d),
        None => dirs_cache().join("event-log-connection-cost"),
    };
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("log.sqlite");

    // The UI's connection: the one that would exist with no runs at all.
    let mut ui = EventLog::open(&path).unwrap();
    ui.create_run(&run_row()).unwrap();
    let base_rss = rss_kb();
    let base_fds = fds();
    println!("baseline (one connection, schema migrated)");
    println!("  RSS {:>8} kB   fds {:>4}", base_rss, base_fds);
    println!();

    println!(
        "{:<14} {:>10} {:>14} {:>8} {:>12}",
        "connections", "RSS (kB)", "per conn (kB)", "fds", "per conn"
    );
    let mut held: Vec<EventLog> = Vec::new();
    for n in 1..=CONNECTIONS {
        let mut log = EventLog::open(&path).unwrap();
        // A run's connection writes; make this one do the same, so the number
        // includes whatever a write allocates and not just an open handle.
        let seq = n as u64;
        log.append_all(&[RawEvent {
            run_id: RunId::from("r"),
            seq,
            ts: "2026-09-14T00:00:00.000Z".to_owned(),
            schema: SCHEMA_VERSION,
            idempotency_key: Some(format!("uuid-{seq}")),
            event: Event::TurnStarted(TurnStarted { index: seq as u32 }),
        }])
        .unwrap();
        let _ = log.last_seq(&RunId::from("r")).unwrap();
        held.push(log);

        if n == 1 || n == 4 || n == 8 || n == CONNECTIONS {
            let rss = rss_kb();
            let f = fds();
            println!(
                "{:<14} {:>10} {:>14.1} {:>8} {:>12.1}",
                n,
                rss,
                (rss - base_rss) as f64 / n as f64,
                f,
                (f - base_fds) as f64 / n as f64,
            );
        }
    }

    drop(held);
    println!();
    println!(
        "after dropping all {CONNECTIONS}:  RSS {:>8} kB   fds {:>4}",
        rss_kb(),
        fds()
    );
    // Closing a connection does not hand its descriptor straight back: SQLite's
    // unix VFS parks descriptors on the inode rather than calling close(2)
    // while the process still holds locks on the file, because POSIX advisory
    // locks are dropped by *any* close on the same inode. They are reused by
    // the next open of the same database, which is what the second round
    // below checks — a run that ends and a run that starts trade the same fd.
    let round_two: Vec<EventLog> = (0..CONNECTIONS)
        .map(|_| EventLog::open(&path).unwrap())
        .collect();
    println!(
        "{CONNECTIONS} opened again:       RSS {:>8} kB   fds {:>4}  (reused, not added)",
        rss_kb(),
        fds()
    );
    drop(round_two);
    let _ = std::fs::remove_dir_all(&dir);
}

fn run_row() -> Run {
    Run {
        id: RunId::from("r"),
        agent: "claude-code".to_owned(),
        agent_version: None,
        provider_session_id: None,
        workspace_path: "/tmp".to_owned(),
        worktree_path: None,
        branch: None,
        shadow_ref: None,
        prompt: None,
        model: None,
        launch_flags: LaunchFlags::default(),
        status: RunStatus::Running,
        started_at: "2026-09-14T00:00:00.000Z".to_owned(),
        finished_at: None,
        parent_run_id: None,
        branch_point_seq: None,
    }
}

fn rss_kb() -> i64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in s.lines() {
        if let Some(v) = line.strip_prefix("VmRSS:") {
            if let Some(n) = v.split_whitespace().next() {
                return n.parse().unwrap_or(-1);
            }
        }
    }
    -1
}

fn fds() -> i64 {
    std::fs::read_dir("/proc/self/fd")
        .map(|d| d.count() as i64)
        .unwrap_or(-1)
}

/// Real storage rather than `std::env::temp_dir`, for the reason
/// `append_cost.rs` records: tmpfs makes a file-backed measurement lie.
fn dirs_cache() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| PathBuf::from("."))
}
