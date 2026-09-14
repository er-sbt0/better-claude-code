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
    ///
    /// The **branch** is untouched too, and that is the half a caller almost
    /// never wants — see [`remove_with_branch`](Self::remove_with_branch).
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

    /// Remove the worktree **and** delete the branch it was checked out on.
    ///
    /// This is what cleaning a run up means, because the two halves of §13's
    /// "a run is created with its worktree and branch" have to come off
    /// together. [`remove`](Self::remove) alone leaves `agent/<run_id>` behind
    /// forever, and a repository that has hosted a few hundred runs is then a
    /// repository whose branch list is a few hundred run ids — invisible on
    /// disk, unmissable in every branch picker the user owns.
    ///
    /// **The order is the reason this is one method rather than two calls.**
    /// git refuses to delete a branch that is checked out in a worktree, so the
    /// branch can only go after the tree does; a caller that did it the other
    /// way round would get a refusal that reads like a bug in the branch name.
    /// And the tree is what holds `self`, so there is no other seam where both
    /// the repository and the branch are still in hand.
    ///
    /// `-D` rather than `-d`, deliberately. An agent's branch is never merged
    /// into anything — that is the whole point of §13's isolation — so `-d`
    /// refuses *every* run this could be called on and the flag would be
    /// decorative. What is actually lost is the branch's own commit objects, if
    /// the agent made any; the file content is not, because §13.1 snapshots the
    /// tree after every mutating tool call onto the shadow ref, which this
    /// leaves alone. `force` is therefore not consulted here: it is §13's
    /// answer to *uncommitted* changes, which is a question about the tree.
    ///
    /// A branch that is already gone is not an error. Cleanup has to be safe to
    /// repeat — a run half-cleaned by a crash, or a branch the user deleted by
    /// hand, is an ordinary state and not one worth failing a second attempt
    /// over.
    pub fn remove_with_branch(self, force: bool) -> Result<()> {
        let repo = self.repo.clone();
        let branch = self.branch.clone();
        self.remove(force)?;
        delete_branch(&repo, &branch)
    }
}

/// Delete `refs/heads/<branch>`, if it is there.
///
/// Private, and it stays private: on its own it is the half of
/// [`Worktree::remove_with_branch`] that fails whenever the ordering is wrong,
/// and a second caller would be a second chance to get that ordering wrong.
fn delete_branch(repo: &Path, branch: &str) -> Result<()> {
    let ref_name = format!("refs/heads/{branch}");
    if !git_ok(repo, ["show-ref", "--verify", "--quiet", &ref_name]) {
        return Ok(());
    }
    git(repo, ["branch", "-D", branch])?;
    Ok(())
}

fn canonical_repo(path: &Path) -> Result<PathBuf> {
    if !git_ok(path, ["rev-parse", "--is-inside-work-tree"]) {
        return Err(Error::NotARepo {
            path: path.display().to_string(),
        });
    }
    Ok(path.canonicalize()?)
}
