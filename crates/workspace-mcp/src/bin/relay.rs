//! `workspace-mcp-relay` — the process Claude Code actually spawns.
//!
//! It is a byte pipe and nothing else. It parses no JSON, knows no method
//! names, and has no opinion about MCP: it reads [`SOCKET_ENV`] from the
//! environment the mcp-config's `env` map gave it, connects, and copies
//! stdin → socket and socket → stdout until one of them ends.
//!
//! That is the whole point. Protocol logic that lived out here would only be
//! reachable by spawning a process, and the workspace's rule — the one that
//! makes `driver::Pipeline` testable against a recorded fixture — is that the
//! logic lives where a test can call it directly. Everything real is in
//! `workspace_mcp::Server`, and this binary is the part that cannot be tested
//! without a process precisely because there is nothing in it to test.
//!
//! **stdout belongs to the protocol.** Nothing here may print to it. Diagnostics
//! go to stderr, where Claude Code collects them as MCP server logs.

use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::thread;

use workspace_mcp::SOCKET_ENV;

fn main() -> ExitCode {
    let Some(path) = std::env::var_os(SOCKET_ENV) else {
        eprintln!(
            "{SOCKET_ENV} is not set. This binary is spawned by an agent through the \
             workspace's own --mcp-config, which supplies it; it is not meant to be run by hand."
        );
        return ExitCode::FAILURE;
    };

    let socket = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot connect to {}: {e}", path.to_string_lossy());
            return ExitCode::FAILURE;
        }
    };
    let Ok(mut to_workspace) = socket.try_clone() else {
        eprintln!("cannot duplicate the socket");
        return ExitCode::FAILURE;
    };
    let mut from_workspace = socket;

    thread::spawn(move || {
        let _ = io::copy(&mut io::stdin().lock(), &mut to_workspace);
        // Our stdin closed: the agent is done talking. Half-close so the
        // workspace's reader sees EOF and lets its connection thread go,
        // instead of holding it until this process exits. Reading continues —
        // a response to the last request may still be in flight.
        let _ = to_workspace.shutdown(Shutdown::Write);
    });

    // `StdoutLock` is line buffered, and every message is one line ending in
    // `\n`, so each response flushes as it is written. Anything that changes
    // this to a `BufWriter` has to flush per line or the agent waits forever
    // for a reply that is sitting in our buffer.
    let mut stdout = io::stdout().lock();
    let mut buf = [0u8; 8192];
    loop {
        match from_workspace.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if stdout.write_all(&buf[..n]).is_err() || stdout.flush().is_err() {
                    break;
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }

    // Deliberately not joining the stdin thread. It is parked in a blocking
    // read on a pipe the parent may never close, and there is no portable way
    // to interrupt that; exiting here drops it with the process. The workspace
    // has closed the connection, so there is nothing left for it to carry.
    ExitCode::SUCCESS
}
