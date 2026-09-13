//! Host the workspace MCP server for something other than the driver.
//!
//! This exists for one job: recording fixtures (design §16). Every `.ndjson` in
//! `fixtures/` is the raw stdout of one real `claude` process, captured with
//! nothing of ours in the pipe — which is exactly what makes it evidence about
//! the *provider* rather than about us. The driver cannot record one, because
//! it consumes stdout to produce events; that is its purpose.
//!
//! So `record.sh` needs a server it can stand up beside a plain `claude`
//! invocation, and this is it: bind the socket, write the flags it implies to a
//! file, and serve until killed.
//!
//!     cargo run -p workspace-mcp --example host -- \
//!         --relay target/debug/workspace-mcp-relay --out /tmp/flags.sh [--deny]
//!
//! The policy is deliberately crude — allow everything, or deny everything.
//! A fixture wants a decision that is the same every time it is recorded, not a
//! realistic one.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use workspace_mcp::{
    qualified, Decision, Listener, PermissionRequest, Workspace, REPORT_FINDING,
    REQUEST_PERMISSION, SYSTEM_PROMPT,
};

struct Fixed {
    allow: bool,
    saw_request: Arc<AtomicBool>,
}

impl Workspace for Fixed {
    fn request_permission(&self, request: PermissionRequest) -> Decision {
        self.saw_request.store(true, Ordering::SeqCst);
        // stderr, never stdout: a recording run has stdout pointed at the
        // fixture, and this process does not share it -- but the habit is the
        // one the relay's header insists on and it costs nothing to keep.
        eprintln!(
            "host: permission {} -> {}",
            request.tool_name,
            if self.allow { "allow" } else { "deny" }
        );
        if self.allow {
            Decision::Allow {
                updated_input: None,
            }
        } else {
            Decision::Deny {
                message: "denied while recording a fixture".to_owned(),
                interrupt: false,
            }
        }
    }

    fn report(&self, tool: &str, payload: serde_json::Value) {
        eprintln!("host: report {tool} {payload}");
    }
}

fn main() -> std::process::ExitCode {
    let mut relay: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut allow = true;

    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--relay" => relay = it.next().map(PathBuf::from),
            "--out" => out = it.next().map(PathBuf::from),
            "--deny" => allow = false,
            "--allow" => allow = true,
            other => {
                eprintln!("unexpected argument: {other}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }

    let (Some(relay), Some(out)) = (relay, out) else {
        eprintln!("usage: host --relay <path> --out <file> [--allow|--deny]");
        return std::process::ExitCode::FAILURE;
    };
    let relay = match relay.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "{}: {e} — build it with `cargo build -p workspace-mcp`",
                relay.display()
            );
            return std::process::ExitCode::FAILURE;
        }
    };

    let dir = std::env::temp_dir().join(format!("agent-workspace-fixture-{}", std::process::id()));
    let saw_request = Arc::new(AtomicBool::new(false));
    let listener = match Listener::bind(
        &dir,
        Arc::new(Fixed {
            allow,
            saw_request: Arc::clone(&saw_request),
        }),
    ) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bind: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    // Shell-sourceable, because the consumer is a bash script. Single quotes
    // around the JSON: it contains double quotes and no single ones.
    let flags = format!(
        "MCP_CONFIG='{}'\nPERMISSION_TOOL='{}'\nREPORT_TOOL='{}'\nSYSTEM_PROMPT='{}'\n",
        listener.mcp_config(&relay),
        qualified(REQUEST_PERMISSION),
        qualified(REPORT_FINDING),
        SYSTEM_PROMPT.replace('\'', "'\\''"),
    );
    if let Err(e) = std::fs::write(&out, flags) {
        eprintln!("{}: {e}", out.display());
        return std::process::ExitCode::FAILURE;
    }
    eprintln!("host: serving on {}", listener.socket_path().display());

    // Serve until stdin closes. `record.sh` holds it open for the life of the
    // `claude` run and closes it after, which is one fewer signal handler than
    // a kill would need and cleans the socket up through `Listener::shutdown`.
    let mut sink = Vec::new();
    let _ = std::io::Read::read_to_end(&mut std::io::stdin(), &mut sink);

    listener.shutdown();
    let _ = std::io::stderr().flush();

    // Exit non-zero when nothing ever asked. A fixture recorded from a run that
    // never reached the server is worse than no fixture: it looks like evidence
    // and records only that the flags parsed.
    if saw_request.load(Ordering::SeqCst) {
        std::process::ExitCode::SUCCESS
    } else {
        eprintln!("host: no permission request ever arrived");
        std::process::ExitCode::from(3)
    }
}
