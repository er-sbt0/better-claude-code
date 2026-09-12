//! The shadow history: how Review attributes a hunk (design §13.1).
//!
//! After every mutating tool call the run's worktree is snapshotted into a
//! commit on a private ref, using a separate index so the user's own index,
//! HEAD and working tree are never touched. A diff hunk is then attributed by
//! blaming that synthetic history: line -> shadow commit -> `seq` -> events.
//!
//! Verified against git 2.48.1 before it was adopted; see
//! `docs/verification/git-shadow-attribution.md` for what was checked, what it
//! costs, and what it does not cover.

use std::ffi::OsStr;
use std::path::PathBuf;

use agent_protocol::RunId;

use crate::error::{Error, Result};
use crate::git::{git, git_env};
use crate::worktree::Worktree;

/// The identity shadow commits are authored under.
///
/// Never the user's. A commit they did not make must not carry their name.
const AUTHOR: &str = "agent-workspace";
const EMAIL: &str = "runtime@agent-workspace.local";

/// What produced a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cause {
    /// The `seq` of the `tool.completed` event this snapshot follows, or of the
    /// `run.started` event for the root snapshot.
    pub seq: u64,
    pub tool: String,
    pub tool_use_id: Option<String>,
}

impl Cause {
    pub fn tool(seq: u64, tool: impl Into<String>, tool_use_id: impl Into<String>) -> Self {
        Cause {
            seq,
            tool: tool.into(),
            tool_use_id: Some(tool_use_id.into()),
        }
    }
}

/// A commit on the shadow ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowCommit {
    pub id: String,
    pub cause: Cause,
}

/// One line of a file, and the snapshot that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    /// 1-based, in the version being blamed.
    pub line: u32,
    pub commit: String,
    pub seq: u64,
    pub tool: String,
    pub tool_use_id: Option<String>,
    pub text: String,
}

/// A run's shadow history.
///
/// `&mut self` on [`ShadowHistory::snapshot`] is load-bearing: the shadow index
/// is a single file and is not concurrency-safe, so snapshots serialize per
/// worktree.
#[derive(Debug)]
pub struct ShadowHistory {
    worktree: PathBuf,
    ref_name: String,
    index: PathBuf,
}

impl ShadowHistory {
    /// Start a run's shadow history with a root snapshot of the worktree.
    ///
    /// **Call this after §13's post-create hook and before the first tool
    /// call.** Rooted any earlier, every file the hook wrote — a dependency
    /// install, codegen — is attributed to the run's first `Edit`. This is not
    /// a style preference; it is the first thing the verification caught.
    pub fn begin(wt: &Worktree, run: &RunId, seq: u64) -> Result<(Self, ShadowCommit)> {
        let index = wt
            .git_dir()
            .join("agent-workspace")
            .join(format!("{}.index", run.0));
        std::fs::create_dir_all(index.parent().expect("has a parent"))?;
        // Seeding from the real index costs a file copy and saves re-hashing
        // every file in the repo on the first snapshot.
        std::fs::copy(wt.git_dir().join("index"), &index)?;

        let mut history = ShadowHistory {
            worktree: wt.path().to_path_buf(),
            ref_name: shadow_ref(run),
            index,
        };
        let root = history
            .commit(&Cause {
                seq,
                tool: "-".to_owned(),
                tool_use_id: None,
            })?
            // The root snapshot has no parent, so its tree is never "unchanged"
            // and a commit is always produced -- even for an empty repository,
            // whose empty tree still differs from no tree at all.
            .expect("the root snapshot always commits");
        Ok((history, root))
    }

    /// Reopen an existing shadow history, for a run this process did not start.
    pub fn open(wt: &Worktree, run: &RunId) -> Result<Self> {
        let index = wt
            .git_dir()
            .join("agent-workspace")
            .join(format!("{}.index", run.0));
        Ok(ShadowHistory {
            worktree: wt.path().to_path_buf(),
            ref_name: shadow_ref(run),
            index,
        })
    }

    /// `refs/agent-workspace/runs/<run_id>`. Store it on the run row: it is
    /// what Review needs, and it outlives the worktree.
    pub fn ref_name(&self) -> &str {
        &self.ref_name
    }

    pub fn tip(&self) -> Result<Option<String>> {
        match git(
            &self.worktree,
            ["rev-parse", "--verify", "-q", &self.ref_name],
        ) {
            Ok(id) => Ok(Some(id)),
            Err(Error::Git { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Snapshot the worktree after a tool call.
    ///
    /// Returns `None` when the tool changed nothing, which is the common case —
    /// most tool calls are reads. Committing an unchanged tree anyway would put
    /// empty commits in the history and make blame attribute lines to the wrong
    /// `seq`.
    pub fn snapshot(&mut self, cause: &Cause) -> Result<Option<ShadowCommit>> {
        self.commit(cause)
    }

    fn commit(&mut self, cause: &Cause) -> Result<Option<ShadowCommit>> {
        let parent = self.tip()?;
        let index: &OsStr = self.index.as_os_str();

        // `git add -A` respects .gitignore. It does not respect anything else,
        // so untracked build output that is not ignored enters the history;
        // $GIT_DIR/info/exclude is where run-scoped excludes belong.
        git_env(&self.worktree, ["add", "-A"], &[("GIT_INDEX_FILE", index)])?;
        let tree = git_env(&self.worktree, ["write-tree"], &[("GIT_INDEX_FILE", index)])?;

        if let Some(parent) = &parent {
            let parent_tree = git(&self.worktree, ["rev-parse", &format!("{parent}^{{tree}}")])?;
            if tree == parent_tree {
                return Ok(None);
            }
        }

        let mut args = vec!["commit-tree".to_owned(), tree];
        if let Some(parent) = &parent {
            args.push("-p".to_owned());
            args.push(parent.clone());
        }
        args.push("-m".to_owned());
        args.push(format!("seq {} {}", cause.seq, cause.tool));
        // Every trailer goes in ONE paragraph: git parses only the last
        // paragraph of a message as trailers, so splitting them across several
        // `-m` flags yields silently empty values on read-back.
        args.push("-m".to_owned());
        args.push(format!(
            "seq: {}\ntool: {}\ntool-use-id: {}",
            cause.seq,
            cause.tool,
            cause.tool_use_id.as_deref().unwrap_or("-"),
        ));
        let id = git_env(
            &self.worktree,
            &args,
            &[
                ("GIT_AUTHOR_NAME", OsStr::new(AUTHOR)),
                ("GIT_AUTHOR_EMAIL", OsStr::new(EMAIL)),
                ("GIT_COMMITTER_NAME", OsStr::new(AUTHOR)),
                ("GIT_COMMITTER_EMAIL", OsStr::new(EMAIL)),
            ],
        )?;
        git(&self.worktree, ["update-ref", &self.ref_name, &id])?;
        Ok(Some(ShadowCommit {
            id,
            cause: cause.clone(),
        }))
    }

    /// Attribute every line of a file's final version to the snapshot that
    /// produced it.
    ///
    /// This answers added and modified hunks. **It cannot answer deleted ones:**
    /// blame only sees lines that survived to the tip. Review must call
    /// [`ShadowHistory::removed_at`] for removed hunks, or it will silently show
    /// no attribution for them.
    pub fn attribute(&self, path: &str) -> Result<Vec<Attribution>> {
        let out = git(
            &self.worktree,
            ["blame", "--line-porcelain", &self.ref_name, "--", path],
        )?;
        let mut out_lines = Vec::new();
        let mut commit = String::new();
        let mut line_no = 0u32;
        for line in out.lines() {
            if let Some(hash) = line.split_whitespace().next() {
                if hash.len() == 40 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
                    commit = hash.to_owned();
                    // The porcelain header is `<sha> <orig-line> <final-line>`.
                    line_no = line
                        .split_whitespace()
                        .nth(2)
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(line_no + 1);
                    continue;
                }
            }
            if let Some(text) = line.strip_prefix('\t') {
                let (seq, tool, tool_use_id) = self.trailers(&commit)?;
                out_lines.push(Attribution {
                    line: line_no,
                    commit: commit.clone(),
                    seq,
                    tool,
                    tool_use_id,
                    text: text.to_owned(),
                });
            }
        }
        Ok(out_lines)
    }

    /// The `seq` of the snapshot that removed `text` from `path`, if any.
    ///
    /// The pickaxe rather than blame, because the line is gone: this is the
    /// newest snapshot whose diff changed the number of occurrences of `text`.
    pub fn removed_at(&self, path: &str, text: &str) -> Result<Option<u64>> {
        let out = git(
            &self.worktree,
            [
                "log",
                "--format=%H",
                "-n",
                "1",
                "-S",
                text,
                &self.ref_name,
                "--",
                path,
            ],
        )?;
        if out.is_empty() {
            return Ok(None);
        }
        Ok(Some(self.trailers(&out)?.0))
    }

    /// Check out the worktree as it stood at a snapshot.
    ///
    /// §20.5 prices run replay as free because of this: it is a `git` command,
    /// not a feature.
    pub fn tree_at(&self, commit: &str, path: &str) -> Result<String> {
        git(&self.worktree, ["show", &format!("{commit}:{path}")])
    }

    fn trailers(&self, commit: &str) -> Result<(u64, String, Option<String>)> {
        let out = git(
            &self.worktree,
            [
                "show",
                "-s",
                "--format=%(trailers:key=seq,valueonly,separator=)%x09\
                  %(trailers:key=tool,valueonly,separator=)%x09\
                  %(trailers:key=tool-use-id,valueonly,separator=)",
                commit,
            ],
        )?;
        let mut parts = out.split('\t');
        let seq = parts.next().unwrap_or("").trim();
        let seq: u64 = seq.parse().map_err(|_| Error::BadTrailer(seq.to_owned()))?;
        let tool = parts.next().unwrap_or("").trim().to_owned();
        let tool_use_id = match parts.next().unwrap_or("").trim() {
            "" | "-" => None,
            s => Some(s.to_owned()),
        };
        Ok((seq, tool, tool_use_id))
    }
}

pub fn shadow_ref(run: &RunId) -> String {
    format!("refs/agent-workspace/runs/{}", run.0)
}
