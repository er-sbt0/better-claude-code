//! What `synchronous = FULL` costs, measured rather than assumed.
//!
//!     cargo run --release -p event-log --example append_cost
//!
//! The log defaults to FULL because §8 names crash recovery as a reason it
//! exists. This exists so that choice stays a measurement and not a belief.
//!
//! It writes to a directory you name, and refuses tmpfs. The first run of this
//! benchmark used `/tmp`, which is tmpfs here, and reported FULL and NORMAL as
//! indistinguishable at 0.02ms — because nothing was ever fsynced to a disk.
//! A durability benchmark on a RAM filesystem measures nothing at all.

use std::time::Instant;

use agent_protocol::{Event, RawEvent, RunId, TurnStarted, SCHEMA_VERSION};
use event_log::{EventLog, Run, RunStatus};

fn main() {
    let dir = match std::env::args().nth(1) {
        Some(d) => std::path::PathBuf::from(d),
        None => dirs_cache().join("event-log-append-cost"),
    };
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let fs_type = filesystem_of(&dir);
    println!("writing to {} ({fs_type})", dir.display());
    assert!(
        fs_type != "tmpfs",
        "{} is tmpfs: nothing is fsynced to a disk there, so FULL and NORMAL \
         would measure the same and the result would be meaningless. \
         Pass a directory on real storage as the first argument.",
        dir.display()
    );
    println!();

    println!(
        "{:<14} {:>12} {:>14} {:>12}",
        "batch size", "commits", "per commit", "events/s"
    );
    for sync in ["FULL", "NORMAL"] {
        println!("\nsynchronous = {sync}");
        for batch in [1usize, 4, 16] {
            let path = dir.join(format!("{sync}-{batch}.sqlite"));
            let _ = std::fs::remove_file(&path);
            let mut log = EventLog::open(&path).unwrap();
            if sync == "NORMAL" {
                log.set_synchronous_normal().unwrap();
            }
            log.create_run(&Run {
                id: RunId::from("r"),
                agent: "claude-code".to_owned(),
                agent_version: None,
                provider_session_id: None,
                workspace_path: "/tmp".to_owned(),
                worktree_path: None,
                branch: None,
                shadow_ref: None,
                prompt: None,
                status: RunStatus::Running,
                started_at: "2026-09-12T00:00:00.000Z".to_owned(),
                finished_at: None,
                parent_run_id: None,
                branch_point_seq: None,
            })
            .unwrap();

            let total = 2000;
            let mut seq = 0u64;
            let start = Instant::now();
            let mut commits = 0;
            while seq < total {
                let events: Vec<RawEvent> = (0..batch)
                    .map(|_| {
                        seq += 1;
                        RawEvent {
                            run_id: RunId::from("r"),
                            seq,
                            ts: "2026-09-12T14:22:08.412Z".to_owned(),
                            schema: SCHEMA_VERSION,
                            idempotency_key: Some(format!("uuid-{seq}")),
                            event: Event::TurnStarted(TurnStarted { index: seq as u32 }),
                        }
                    })
                    .collect();
                log.append_all(&events).unwrap();
                commits += 1;
            }
            let el = start.elapsed();
            println!(
                "{:<14} {:>12} {:>11.2}ms {:>12.0}",
                format!("{batch} event(s)"),
                commits,
                el.as_secs_f64() * 1000.0 / commits as f64,
                total as f64 / el.as_secs_f64(),
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A cache directory on real storage, rather than `std::env::temp_dir`, which
/// is exactly the trap this benchmark fell into.
fn dirs_cache() -> std::path::PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

fn filesystem_of(dir: &std::path::Path) -> String {
    let out = std::process::Command::new("df")
        .arg("--output=fstype")
        .arg(dir)
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .lines()
            .nth(1)
            .unwrap_or("unknown")
            .trim()
            .to_owned(),
        Err(_) => "unknown".to_owned(),
    }
}
