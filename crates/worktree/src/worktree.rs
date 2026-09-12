//! One worktree per run (design §13).

use std::path::{Path, PathBuf};

use agent_protocol::RunId;

use crate::error::{Error, Result};
use crate::git::{git, git_ok};

/// A run's isolated checkout.
///
/// §13: a run is created *with* its worktree and branch, and the user's own
/// checkout is never an agent's working directory. A second, unadvertised
/// consequence matters for §13.1 — a fresh worktree contains no uncommitted
/// work of the user's, so nothing of theirs can be swept into a run's shadow
/// history even in principle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    repo: PathBuf,
    path: PathBuf,
    branch: String,
    /// This worktree's own git dir — `<repo>/.git/worktrees/<name>`, not the
    /// shared one. The shadow index lives here so it dies with the worktree.
    git_dir: PathBuf,
}

impl Worktree {
    /// Create the worktree and its branch at `base`.
    ///
    /// `base` is any commit-ish; pass the branch point the run should start
    /// from. The default layout is `<repo>/.worktrees/<run_id>`.
    pub fn create(repo: &Path, run: &RunId, branch: &str, base: &str) -> Result<Self> {
        let repo = canonical_repo(repo)?;
        let path = repo.join(".worktrees").join(&run.0);
        if path.exists() {
            return Err(Error::WorktreeExists {
                path: path.display().to_string(),
            });
        }
        std::fs::create_dir_all(path.parent().expect("has a parent"))?;
        git(
            &repo,
            [
                "worktree",
                "add",
                "-b",
                branch,
                &path.to_string_lossy(),
                base,
            ],
        )?;
        let git_dir = PathBuf::from(git(&path, ["rev-parse", "--absolute-git-dir"])?);
        Ok(Worktree {
            repo,
            path,
            branch: branch.to_owned(),
            git_dir,
        })
    }

    /// Adopt a worktree that already exists — after a restart, for instance,
    /// when the run row says where it is but this process has never seen it.
    pub fn open(path: &Path) -> Result<Self> {
        if !git_ok(path, ["rev-parse", "--is-inside-work-tree"]) {
            return Err(Error::NotARepo {
                path: path.display().to_string(),
            });
        }
        let path = path.canonicalize()?;
        let git_dir = PathBuf::from(git(&path, ["rev-parse", "--absolute-git-dir"])?);
        let repo = PathBuf::from(git(
            &path,
            ["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| path.clone());
        let branch = git(&path, ["rev-parse", "--abbrev-ref", "HEAD"])?;
        Ok(Worktree {
            repo,
            path,
            branch,
            git_dir,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn repo(&self) -> &Path {
        &self.repo
    }
    pub fn branch(&self) -> &str {
        &self.branch
    }
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// Whether anything in the worktree is uncommitted.
    ///
    /// §13: never delete a worktree with uncommitted changes without
    /// confirmation. That confirmation is the caller's to ask for; this is how
    /// it knows to.
    pub fn is_dirty(&self) -> Result<bool> {
        Ok(!git(&self.path, ["status", "--porcelain"])?.is_empty())
    }

    /// Remove the worktree.
    ///
    /// The run's shadow history is untouched: it is held by a ref in the shared
    /// namespace, so Review keeps its attribution for a run whose worktree is
    /// long gone (§13.1, verified against `gc --prune=now`).
    pub fn remove(self, force: bool) -> Result<()> {
        let path = self.path.to_string_lossy().into_owned();
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(&path);
        git(&self.repo, args)?;
        Ok(())
    }
}

fn canonical_repo(path: &Path) -> Result<PathBuf> {
    if !git_ok(path, ["rev-parse", "--is-inside-work-tree"]) {
        return Err(Error::NotARepo {
            path: path.display().to_string(),
        });
    }
    Ok(path.canonicalize()?)
}
