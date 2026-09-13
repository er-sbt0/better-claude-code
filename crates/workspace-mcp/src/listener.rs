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
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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

/// The bound socket, the thread accepting on it, and the connections it opened.
///
/// Dropping it unlinks the socket, so a driver that panics does not leave a
/// file behind that the next run has to reason about.
pub struct Listener {
    path: PathBuf,
    accepting: Arc<AtomicBool>,
    conns: Conns,
    accept: Option<JoinHandle<()>>,
}

/// A connection being served, and the handle that ends it.
///
/// The stream is a second descriptor on the same socket, kept only so that
/// [`Listener::stop`] can shut it down: a thread parked in `read` on a peer
/// that never speaks again cannot be joined, and half-closing under it is what
/// turns that block into an EOF.
struct Conn {
    stream: UnixStream,
    thread: JoinHandle<()>,
}

type Conns = Arc<Mutex<Vec<Conn>>>;

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
        let conns: Conns = Arc::new(Mutex::new(Vec::new()));
        let accept = {
            let accepting = Arc::clone(&accepting);
            let conns = Arc::clone(&conns);
            thread::spawn(move || accept_loop(&listener, &workspace, &accepting, &conns))
        };

        Ok(Listener {
            path,
            accepting,
            conns,
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

    /// Close the door: stop accepting, end every open connection, join every
    /// thread this listener started, and unlink the socket.
    ///
    /// **When this returns, no thread of this server is running**, so the
    /// `Arc<dyn Workspace>` the caller handed to [`bind`](Self::bind) is the
    /// last one in existence and whatever that workspace was holding can be
    /// reclaimed. That is not a nicety: the driver's [`Workspace`] owns the
    /// run's only connection to the event log, and `Session::wait` takes it
    /// back by unwrapping the `Arc`. Leaving one connection thread running
    /// would make that a race, won most of the time, and a run that lost it
    /// would sit in the database saying `Running` forever.
    ///
    /// An open connection is **shut down** rather than waited for. A relay
    /// holds its connection for the life of its `claude` process, so waiting
    /// for the peer would mean the workspace could not close the door until the
    /// agent it is shutting down had finished — which is the hang this method
    /// exists to avoid. Half-closing the socket under the serving thread turns
    /// its blocking read into an EOF, so the join that follows is bounded by
    /// the one line that thread might be in the middle of writing.
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

        // Only now: the accept thread is the only thing that appends here, so
        // after joining it this list cannot grow and draining it once is
        // enough.
        let open: Vec<Conn> = std::mem::take(&mut *lock(&self.conns));
        for conn in &open {
            let _ = conn.stream.shutdown(Shutdown::Both);
        }
        for conn in open {
            let _ = conn.thread.join();
        }

        let _ = fs::remove_file(&self.path);
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop();
    }
}

fn accept_loop(
    listener: &UnixListener,
    workspace: &Arc<dyn Workspace>,
    accepting: &AtomicBool,
    conns: &Conns,
) {
    for stream in listener.incoming() {
        if !accepting.load(Ordering::SeqCst) {
            return;
        }
        match stream {
            Ok(stream) => {
                // A second descriptor on the same socket, taken before the
                // stream is moved into its thread. Without it there is no way
                // to end a connection whose peer has stopped talking, and
                // `stop` would have to choose between waiting for the agent
                // and leaving a thread holding the workspace.
                let Ok(handle) = stream.try_clone() else {
                    continue;
                };
                // One thread per connection. Claude Code spawns one relay per
                // session, but nothing in the protocol says so, and a second
                // connection must not be silently ignored — it would look like
                // a hung agent rather than a refused connection.
                let workspace = Arc::clone(workspace);
                let thread = thread::spawn(move || serve(stream, workspace));
                let mut open = lock(conns);
                // A long run can reconnect; finished threads are joined here so
                // the list tracks what is actually open rather than every
                // connection the run ever made.
                open.retain(|c: &Conn| !c.thread.is_finished());
                open.push(Conn {
                    stream: handle,
                    thread,
                });
            }
            // A failed accept is per-connection (the peer went away between
            // connect and accept, or the process is out of descriptors); it
            // says nothing about the listener, so the loop continues.
            Err(_) => continue,
        }
    }
}

/// A poisoned connection list means a thread panicked while holding it, which
/// can only have been between a `push` and its guard dropping. Recovering is
/// right: the alternative is a listener that can never be shut down.
fn lock(conns: &Conns) -> std::sync::MutexGuard<'_, Vec<Conn>> {
    conns.lock().unwrap_or_else(|e| e.into_inner())
}

fn serve(stream: UnixStream, workspace: Arc<dyn Workspace>) {
    // Half-close on every exit from this function, and not merely on the error
    // paths.
    //
    // `Listener` keeps a second descriptor on this socket so that `stop` can
    // end a connection whose peer has gone quiet. That descriptor is also the
    // reason dropping the stream here is no longer enough to close the
    // connection: the kernel sends FIN when the *last* descriptor goes, so a
    // serving thread that finished normally would leave the peer reading a
    // socket nobody was ever going to write to again. A relay waits on that
    // socket for the life of its `claude` process, so the process would never
    // exit and neither would whoever was waiting on it.
    struct HangUp(UnixStream);
    impl Drop for HangUp {
        fn drop(&mut self) {
            let _ = self.0.shutdown(Shutdown::Both);
        }
    }

    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let Ok(hangup) = stream.try_clone() else {
        return;
    };
    let _hangup = HangUp(hangup);
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
