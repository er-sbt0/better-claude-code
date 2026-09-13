//! The unix socket the driver holds and the relay dials.
//!
//! This exists because of one fact about Claude Code: it spawns stdio MCP
//! servers as its own child processes. The server therefore cannot be a thread
//! in the driver that Claude Code somehow talks to — something has to be on the
//! other end of *its* child's stdio. That something is `workspace-mcp-relay`,
//! and everything in this module is the door it knocks on.
//!
//! Threads, not an async runtime, for the reason `driver::process` gives: the
//! work is one accept loop plus one reader per connection, and a runtime to
//! express that would be the largest dependency in the workspace.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use serde_json::json;

use crate::error::{Error, Result};
use crate::server::{Server, Workspace};
use crate::{SERVER_NAME, SOCKET_ENV};

const SOCKET_FILE: &str = "mcp.sock";

/// `sun_path` in `sockaddr_un` is 108 bytes on Linux including the terminator,
/// so a socket path is capped well below `PATH_MAX`. Worth checking ourselves:
/// the kernel reports an over-long path as `EINVAL` — "invalid argument" — with
/// nothing to say which argument, and a workspace nested a few directories deep
/// hits it for real.
const SUN_PATH_MAX: usize = 107;

/// The bound socket and the thread accepting on it.
///
/// Dropping it unlinks the socket, so a driver that panics does not leave a
/// file behind that the next run has to reason about.
pub struct Listener {
    path: PathBuf,
    accepting: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl Listener {
    /// Bind `<dir>/mcp.sock`, creating `dir` at mode 0700 if it does not exist.
    ///
    /// **`dir` must not be inside the run's worktree** (§15). The agent has
    /// write access to every path in its worktree, and a socket it can unlink,
    /// replace with a file, or bind its own listener over is a socket through
    /// which it can answer its own permission requests. The natural home is the
    /// workspace's own state directory, keyed by run id.
    ///
    /// A socket file left behind by a crash is removed rather than reported:
    /// bind fails with `EADDRINUSE` on a stale file just as it does on a live
    /// one, and the difference is whether anything answers a connection to it.
    /// Refusing to start would make one crash permanent.
    pub fn bind(dir: &Path, workspace: Arc<dyn Workspace>) -> Result<Listener> {
        fs::create_dir_all(dir)?;
        // Set every time, not only on create: a directory that already exists
        // with looser permissions is exactly the case this is defending
        // against.
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;

        let path = dir.join(SOCKET_FILE);
        let len = path.as_os_str().len();
        if len > SUN_PATH_MAX {
            return Err(Error::SocketPathTooLong {
                path: path.display().to_string(),
                len,
                limit: SUN_PATH_MAX,
            });
        }

        // `symlink_metadata`, so a symlink planted at the path is seen as a
        // thing to remove rather than followed to whatever it points at.
        if fs::symlink_metadata(&path).is_ok() {
            if UnixStream::connect(&path).is_ok() {
                return Err(Error::SocketInUse {
                    path: path.display().to_string(),
                });
            }
            fs::remove_file(&path)?;
        }

        let listener = UnixListener::bind(&path).map_err(|source| Error::Bind {
            path: path.display().to_string(),
            source,
        })?;
        // The 0700 on the directory is the real control; this is the belt to
        // its braces, and it does not depend on the caller's umask.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;

        let accepting = Arc::new(AtomicBool::new(true));
        let accept = {
            let accepting = Arc::clone(&accepting);
            thread::spawn(move || accept_loop(&listener, &workspace, &accepting))
        };

        Ok(Listener {
            path,
            accepting,
            accept: Some(accept),
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.path
    }

    /// The value for `--mcp-config`, as inline JSON.
    ///
    /// `--mcp-config` takes a file path *or* inline JSON, discriminated
    /// textually: a value whose trimmed text starts with `{` is parsed as JSON.
    /// So there is no temp file here — which is not only convenience. A config
    /// file is a file, and a file is one more thing with a lifetime, a
    /// permission mode and a reader.
    ///
    /// `relay` must be the absolute path to the `workspace-mcp-relay` binary.
    /// The `env` map is the whole point of this shape: `driver::process::spawn`
    /// gives the `claude` child no environment of its own, so this is the only
    /// channel by which the socket path reaches the relay.
    pub fn mcp_config(&self, relay: &Path) -> String {
        // Lossy because JSON has no other option for a non-UTF-8 path. A path
        // that needs it would fail at `connect` anyway, and loudly.
        let config = json!({
            "mcpServers": {
                SERVER_NAME: {
                    // Optional on the stdio variant and stated anyway: it is
                    // the discriminator for every other variant, and a reader
                    // of the argv should not have to know that this one is
                    // special. Note it is `type`, not `transport` — that is the
                    // spelling of the CLI flag on `claude mcp add`, and the
                    // binary rejects it here.
                    "type": "stdio",
                    "command": relay.to_string_lossy(),
                    "args": [],
                    "env": { SOCKET_ENV: self.path.to_string_lossy() },
                }
            }
        });
        serde_json::to_string(&config).expect("a serde_json::Value always serializes")
    }

    /// Stop accepting, join the accept thread, and unlink the socket.
    ///
    /// Connections already open are **not** joined. A relay holds its
    /// connection for the life of its `claude` process, so joining would mean
    /// the workspace could not close the door until the agent it is shutting
    /// down had finished — and those threads end on their own when the peer's
    /// socket closes.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        let Some(accept) = self.accept.take() else {
            return;
        };
        self.accepting.store(false, Ordering::SeqCst);
        // Wake the blocking `accept`. Without an async runtime or a
        // nonblocking poll loop that burns CPU while a run is idle, a
        // self-connection is the way to make a thread parked in the kernel
        // notice a flag. The loop checks the flag before it looks at the
        // connection, so this one is never served.
        let _ = UnixStream::connect(&self.path);
        let _ = accept.join();
        let _ = fs::remove_file(&self.path);
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop();
    }
}

fn accept_loop(listener: &UnixListener, workspace: &Arc<dyn Workspace>, accepting: &AtomicBool) {
    for stream in listener.incoming() {
        if !accepting.load(Ordering::SeqCst) {
            return;
        }
        match stream {
            Ok(stream) => {
                // One thread per connection. Claude Code spawns one relay per
                // session, but nothing in the protocol says so, and a second
                // connection must not be silently ignored — it would look like
                // a hung agent rather than a refused connection.
                let workspace = Arc::clone(workspace);
                thread::spawn(move || serve(stream, workspace));
            }
            // A failed accept is per-connection (the peer went away between
            // connect and accept, or the process is out of descriptors); it
            // says nothing about the listener, so the loop continues.
            Err(_) => continue,
        }
    }
}

fn serve(stream: UnixStream, workspace: Arc<dyn Workspace>) {
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    // Per connection, not shared: the server holds nothing but the `Arc`, and
    // all the state that matters lives behind the `Workspace`.
    let server = Server::new(workspace);
    let mut writer = stream;

    for line in BufReader::new(read_half).lines() {
        // A read error is a closed or broken connection. There is no
        // resynchronizing a stream we cannot read, and the relay will be
        // restarted with its agent.
        let Ok(line) = line else { return };
        let Some(response) = server.handle_line(&line) else {
            continue;
        };
        // The response never contains a newline of its own — `to_string`
        // escapes them — so one `writeln!` is one message.
        if writeln!(writer, "{response}").is_err() || writer.flush().is_err() {
            return;
        }
    }
}
