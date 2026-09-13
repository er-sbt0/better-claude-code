//! Drive one real Claude Code run and watch it.
//!
//!     cargo run -p driver --example run -- --repo <path> "<prompt>"
//!
//! The first thing in this repository that can be pointed at a real agent. It
//! is a demonstration of the spine, not a product: no UI, and no permission
//! dialog — but with `--ask` there is now a permission *channel*, which is the
//! thing §12 option A needs and the thing a dialog would sit on top of.
//!
//! Without `--ask`, everything that would prompt is denied outright
//! (`--permission-prompts none`), so a run is safe to leave alone and honest
//! about what it could not do.
//!
//!     --repo <path>       repository to run in (required)
//!     --model <id>        e.g. claude-sonnet-5
//!     --allow <tool>      repeatable, e.g. --allow Read --allow Edit
//!     --mode <mode>       --permission-mode, e.g. acceptEdits
//!     --ask <policy>      host the workspace MCP server and answer prompts:
//!                         `allow` or `deny` (default: no server, deny-prompts)
//!     --relay <path>      the relay binary (default: beside this example)
//!     --max-turns <n>     default 6
//!     --budget <usd>      default 1.00
//!     --db <path>         event log (default <repo>/.agent-workspace/log.sqlite)
//!     --keep              do not remove the worktree afterwards

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;

use agent_protocol::{Event, RunId};
use driver::{Decider, LaunchSpec, Permission, RunSpec, WorkspaceMcp};
use event_log::EventLog;
use workspace_mcp::{Decision, PermissionRequest};
use worktree::{ShadowHistory, Worktree};

/// Answer every request the same way, and say so on stdout.
///
/// A real policy belongs in the UI (§12's V1 surface is a dialog). This exists
/// to prove the channel carries a decision in both directions, which is the
/// thing that had never been demonstrated.
struct Fixed {
    allow: bool,
}

impl Decider for Fixed {
    fn decide(&self, request: &PermissionRequest) -> Decision {
        let args = request
            .input
            .as_object()
            .map(|m| {
                let mut k: Vec<&str> = m.keys().map(String::as_str).collect();
                k.sort();
                format!("{{{}}}", k.join(","))
            })
            .unwrap_or_else(|| "-".to_owned());
        println!(
            "      ?  permission  {} {} -> {}",
            request.tool_name,
            args,
            if self.allow { "allow" } else { "deny" }
        );
        if self.allow {
            Decision::Allow {
                updated_input: None,
            }
        } else {
            Decision::Deny {
                message: "denied by --ask deny".to_owned(),
                interrupt: false,
            }
        }
    }
}

/// The relay lives beside whatever binary is running this example.
fn relay_beside_me() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // target/<profile>/examples/run -> target/<profile>/workspace-mcp-relay
    let candidate = exe.parent()?.parent()?.join("workspace-mcp-relay");
    candidate.exists().then_some(candidate)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut repo = None;
    let mut model = None;
    let mut allow: Vec<String> = Vec::new();
    let mut mode: Option<String> = None;
    let mut max_turns = 6u32;
    let mut budget = 1.0f64;
    let mut db: Option<PathBuf> = None;
    let mut keep = false;
    let mut ask: Option<String> = None;
    let mut relay: Option<PathBuf> = None;
    let mut prompt = None;

    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--repo" => repo = it.next().map(PathBuf::from),
            "--model" => model = it.next(),
            "--allow" => allow.extend(it.next()),
            "--mode" => mode = it.next(),
            "--max-turns" => max_turns = it.next().and_then(|s| s.parse().ok()).unwrap_or(6),
            "--budget" => budget = it.next().and_then(|s| s.parse().ok()).unwrap_or(1.0),
            "--db" => db = it.next().map(PathBuf::from),
            "--ask" => ask = it.next(),
            "--relay" => relay = it.next().map(PathBuf::from),
            "--keep" => keep = true,
            "-h" | "--help" => return usage(),
            _ if prompt.is_none() => prompt = Some(a),
            _ => return die(&format!("unexpected argument: {a}")),
        }
    }
    let (Some(repo), Some(prompt)) = (repo, prompt) else {
        return usage();
    };
    let repo = match repo.canonicalize() {
        Ok(p) => p,
        Err(e) => return die(&format!("{}: {e}", repo.display())),
    };

    let db = db.unwrap_or_else(|| repo.join(".agent-workspace/log.sqlite"));
    if let Some(parent) = db.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut log = match EventLog::open(&db) {
        Ok(l) => l,
        Err(e) => return die(&format!("{}: {e}", db.display())),
    };

    // §8's crash recovery: anything the log still calls running died with a
    // previous process.
    match driver::reconcile_after_restart(&mut log, &now()) {
        Ok(ids) if !ids.is_empty() => {
            println!(
                "reconciled {} run(s) abandoned by an earlier exit",
                ids.len()
            );
        }
        Err(e) => return die(&format!("reconcile: {e}")),
        _ => {}
    }

    let workspace_mcp = match ask.as_deref() {
        None => None,
        Some(policy @ ("allow" | "deny")) => {
            let Some(relay) = relay.or_else(relay_beside_me) else {
                return die(
                    "--ask needs the relay binary: run `cargo build -p workspace-mcp` \
                     or pass --relay <path>",
                );
            };
            println!("relay   {}", relay.display());
            Some(WorkspaceMcp {
                relay,
                runtime_dir: driver::default_runtime_dir(),
                decider: Arc::new(Fixed {
                    allow: policy == "allow",
                }),
            })
        }
        Some(other) => return die(&format!("--ask takes `allow` or `deny`, got {other}")),
    };

    let run_id = RunId::from(format!("run_{}", uuid_like()));
    let mut launch = LaunchSpec::new(&repo, driver::new_session_id(), Permission::DenyPrompts);
    launch.model = model;
    launch.allowed_tools = allow;
    launch.permission_mode = mode;
    launch.max_turns = Some(max_turns);
    launch.max_budget_usd = Some(budget);

    let spec = RunSpec {
        run_id: run_id.clone(),
        repo: repo.clone(),
        branch: format!("agent/{}", run_id.0),
        base: "HEAD".to_owned(),
        prompt: prompt.clone(),
        post_create_hook: None,
        launch,
        workspace_mcp,
    };

    let (tx, rx) = mpsc::channel();
    println!("run     {}", run_id.0);
    println!("repo    {}", repo.display());
    println!("log     {}", db.display());
    println!("prompt  {prompt}\n");

    let started = match driver::start(log, spec, &now(), Some(tx)) {
        Ok(s) => s,
        Err(e) => return die(&format!("start: {e}")),
    };
    println!("worktree {}", started.tree.worktree.path().display());
    println!("shadow   {}\n", started.tree.shadow_ref);
    let wt_path = started.tree.worktree.path().to_path_buf();

    for ev in rx {
        println!("{:>4}  {}", ev.seq, describe(&ev.event));
        // Nothing else will end the process: with `--input-format stream-json`
        // the agent waits for another turn until stdin closes.
        if matches!(ev.event, Event::TurnFinished(_)) {
            let _ = started.session.close_input();
        }
    }

    let (outcome, log) = match started.session.wait() {
        Ok(x) => x,
        Err(e) => return die(&format!("wait: {e}")),
    };

    println!("\nexit {:?}", outcome.exit_code);
    if !outcome.stderr.trim().is_empty() {
        println!("stderr:\n{}", outcome.stderr.trim());
    }
    match log.metrics(&run_id) {
        Ok(m) => println!(
            "{} turns · {} tool calls · {} denials · ${:.4} · {} in / {} out tokens",
            m.turns,
            m.tool_calls,
            m.permission_denials,
            m.cost_usd,
            m.input_tokens,
            m.output_tokens
        ),
        Err(e) => println!("metrics: {e}"),
    }
    if let Ok(h) = log.health(&run_id) {
        println!(
            "{} events stored, {} unclassified ({:.1}%)",
            h.events,
            h.unclassified,
            h.unclassified_ratio() * 100.0
        );
    }

    show_attribution(&wt_path, &run_id);

    if keep {
        println!("\nworktree kept at {}", wt_path.display());
    } else if let Ok(wt) = Worktree::open(&wt_path) {
        // §13: never discard uncommitted work silently.
        match wt.is_dirty() {
            Ok(true) => println!(
                "\nworktree has uncommitted changes and was kept: {}",
                wt_path.display()
            ),
            _ => {
                let _ = wt.remove(false);
                println!("\nworktree removed; the shadow history survives it");
            }
        }
    }
}

/// §13.1, made visible: every changed line beside the tool call that produced
/// it. This is what Review renders.
fn show_attribution(wt: &std::path::Path, run: &RunId) {
    let Ok(worktree) = Worktree::open(wt) else {
        return;
    };
    let Ok(shadow) = ShadowHistory::open(&worktree, run) else {
        return;
    };
    let Ok(Some(tip)) = shadow.tip() else {
        return;
    };
    let root = std::process::Command::new("git")
        .current_dir(wt)
        .args(["rev-list", "--max-parents=0", &tip])
        .output();
    let Ok(root) = root else { return };
    let root = String::from_utf8_lossy(&root.stdout).trim().to_owned();

    let changed = std::process::Command::new("git")
        .current_dir(wt)
        .args(["diff", "--name-only", &root, &tip])
        .output();
    let Ok(changed) = changed else { return };
    let files: Vec<String> = String::from_utf8_lossy(&changed.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    if files.is_empty() {
        println!("\nno files changed");
        return;
    }

    println!("\nchanged files, attributed:");
    for f in files {
        let Ok(attr) = shadow.attribute(&f) else {
            println!("  {f}  (attribution unavailable)");
            continue;
        };
        // Only the lines this run is responsible for; seq 0 is how the worktree
        // arrived.
        let mut by_seq: Vec<(u64, String, usize)> = Vec::new();
        for a in attr.iter().filter(|a| a.seq > driver::SEQ_BEFORE_RUN) {
            match by_seq.last_mut() {
                Some((s, _, n)) if *s == a.seq => *n += 1,
                _ => by_seq.push((a.seq, a.tool.clone(), 1)),
            }
        }
        if by_seq.is_empty() {
            continue;
        }
        println!("  {f}");
        for (seq, tool, lines) in by_seq {
            println!("      {lines:>4} line(s)  ← seq {seq} ({tool})");
        }
    }
}

fn describe(e: &Event) -> String {
    match e {
        Event::RunStarted(p) => format!("run.started       {} {}", p.agent, opt(&p.model)),
        Event::RunFinished(p) => format!("run.finished      turns={}", p.turns),
        Event::RunFailed(p) => format!("run.failed        {}", p.reason),
        Event::RunRateLimited(_) => "run.rate_limited".to_owned(),
        Event::TurnStarted(p) => format!("turn.started      index={}", p.index),
        Event::TurnFinished(p) => format!(
            "turn.finished     {} ${:.4} denials={}",
            p.subtype, p.usage.cost_usd, p.permission_denials
        ),
        Event::UserMessage(p) if p.is_replay => {
            format!("user.message      (ack) {}", clip(&p.text))
        }
        Event::UserMessage(p) => format!("user.message      {}", clip(&p.text)),
        Event::AssistantMessage(p) => format!("assistant.message {}", clip(&p.text)),
        Event::AssistantThinking(p) => format!("assistant.think   {}", clip(&p.text)),
        Event::ToolStarted(p) => format!(
            "tool.started      {} {}",
            p.name,
            clip(&p.input.to_string())
        ),
        Event::ToolCompleted(p) => format!(
            "tool.completed    {}{}{}",
            p.name.as_deref().unwrap_or("?"),
            if p.is_error { " ERROR" } else { "" },
            p.non_execution
                .as_deref()
                .map(|r| format!(" ({r})"))
                .unwrap_or_default()
        ),
        Event::PermissionRequested(p) => format!("permission.req    {}", p.tool_name),
        Event::PermissionResolved(p) => format!(
            "permission.res    {} allowed={} {}",
            p.tool_name,
            p.allowed,
            p.reason_type.as_deref().unwrap_or("")
        ),
        Event::WorkspaceReport(p) => format!(
            "workspace.report  {} {}",
            p.tool,
            clip(&p.payload.to_string())
        ),
        Event::ProcessOutput(p) => format!("process.output    {}", clip(&p.text)),
        Event::Unclassified(p) => format!(
            "raw.unclassified  {:?} {}",
            p.reason,
            p.source_type.as_deref().unwrap_or("")
        ),
    }
}

fn clip(s: &str) -> String {
    let one = s.replace('\n', " ");
    if one.chars().count() <= 80 {
        return one;
    }
    format!("{}…", one.chars().take(79).collect::<String>())
}

fn opt(s: &Option<String>) -> &str {
    s.as_deref().unwrap_or("-")
}

fn now() -> String {
    // The driver takes timestamps from its caller (§8), and this example has no
    // clock dependency of its own to spend.
    std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%S.000Z"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_owned())
}

fn uuid_like() -> String {
    driver::new_session_id().0.replace('-', "")[..12].to_owned()
}

fn usage() {
    eprintln!(
        "usage: cargo run -p driver --example run -- --repo <path> [options] \"<prompt>\"\n\
         \n\
             --repo <path>      repository to run in (required)\n\
             --model <id>       e.g. claude-sonnet-5\n\
             --allow <tool>     repeatable: --allow Read --allow Edit\n\
             --mode <mode>      --permission-mode, e.g. acceptEdits\n\
             --max-turns <n>    default 6\n\
             --budget <usd>     default 1.00\n\
             --db <path>        event log (default <repo>/.agent-workspace/log.sqlite)\n\
             --keep             do not remove the worktree afterwards\n\
         \n\
         Nothing may prompt: anything requiring permission is denied (§12).\n\
         Grant tools explicitly with --allow."
    );
}

fn die(msg: &str) {
    eprintln!("error: {msg}");
    std::process::exit(1);
}
