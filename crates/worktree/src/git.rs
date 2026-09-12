//! Running `git`.
//!
//! A subprocess rather than libgit2, deliberately: §13.1 was verified by
//! observing what the `git` binary does
//! (`docs/verification/git-shadow-attribution.md`), and a second
//! implementation of the same semantics would be a second thing to verify.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

use crate::error::{Error, Result};

/// Run `git` in `cwd` and return stdout, trimmed of its trailing newline.
pub(crate) fn git<I, S>(cwd: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run(cwd, args, &[])
}

/// As [`git`], with extra environment. Used for `GIT_INDEX_FILE` and for the
/// identity the shadow commits are authored under — which is ours, never the
/// user's, so their name never appears on a commit they did not make.
pub(crate) fn git_env<I, S>(cwd: &Path, args: I, env: &[(&str, &OsStr)]) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run(cwd, args, env)
}

fn run<I, S>(cwd: &Path, args: I, env: &[(&str, &OsStr)]) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd);
    let args: Vec<String> = args
        .into_iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect();
    cmd.args(&args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().map_err(|source| Error::Spawn {
        args: args.join(" "),
        source,
    })?;
    if !out.status.success() {
        return Err(Error::Git {
            args: args.join(" "),
            status: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        });
    }
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    while s.ends_with('\n') || s.ends_with('\r') {
        s.pop();
    }
    Ok(s)
}

/// Whether a git invocation succeeds, for the questions where failure is an
/// answer rather than a problem.
pub(crate) fn git_ok<I, S>(cwd: &Path, args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    git(cwd, args).is_ok()
}
