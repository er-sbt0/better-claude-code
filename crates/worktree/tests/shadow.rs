//! The §13.1 mechanism, as a living guard.
//!
//! `docs/verification/git-shadow-attribution.md` is the dated record of what
//! was checked against git 2.48.1 and what it cost. These are the same
//! assertions in CI, so a git upgrade that changes the behaviour breaks a test
//! rather than Review.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use agent_protocol::RunId;
use worktree::{Cause, ShadowHistory, Worktree};

// ------------------------------------------------------------------ fixture --

struct Fixture {
    _home: TempDir,
    repo: PathBuf,
}

impl Fixture {
    /// A repo with one commit, plus in-flight work of the user's: one staged
    /// edit and one untracked file.
    fn new(name: &str) -> Self {
        let home = TempDir::new(name);
        let repo = home.path().join("my-app");
        fs::create_dir_all(&repo).unwrap();

        isolate_git_config();
        git(&repo, &["init", "-q", "."]);
        git(&repo, &["config", "user.name", "The User"]);
        git(&repo, &["config", "user.email", "user@example.com"]);
        write(&repo, "src/auth.ts", AUTH_TS);
        write(&repo, ".gitignore", "node_modules/\n");
        write(&repo, "node_modules/x", "junk\n");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "base"]);

        append(&repo, "src/auth.ts", "// the user's own scratch note\n");
        git(&repo, &["add", "src/auth.ts"]);
        write(&repo, "SCRATCH.md", "untracked user file\n");

        Fixture { _home: home, repo }
    }

    fn head(&self) -> String {
        git(&self.repo, &["rev-parse", "HEAD"])
    }
    fn staged_tree(&self) -> String {
        git(&self.repo, &["write-tree"])
    }
    fn index_mtime(&self) -> std::time::SystemTime {
        fs::metadata(self.repo.join(".git/index"))
            .unwrap()
            .modified()
            .unwrap()
    }
}

const AUTH_TS: &str = "\
export function refresh(token: string) {
  return fetch(\"/refresh\", { method: \"POST\", body: token });
}

export function logout() {
  return fetch(\"/logout\");
}
";

/// Point git at one throwaway global config for the whole test process.
///
/// `GIT_CONFIG_GLOBAL` is process-wide state and these tests run in parallel,
/// so it is set exactly once, to a file that outlives every fixture. Setting it
/// per fixture races: one test's `TempDir` drop deletes the config another test
/// is still spawning git with.
///
/// It exists at all because the developer's own `~/.gitconfig` would otherwise
/// decide what these tests measure — `core.excludesFile` and `init.templateDir`
/// in particular.
fn isolate_git_config() {
    static ONCE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("worktree-test-cfg-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("gitconfig");
        fs::write(&cfg, "[init]\n\tdefaultBranch = main\n").unwrap();
        // Safe here: `get_or_init` blocks every other test thread until this
        // returns, and each one calls it before spawning any git.
        std::env::set_var("GIT_CONFIG_GLOBAL", &cfg);
        std::env::set_var("GIT_CONFIG_SYSTEM", "/dev/null");
        cfg
    });
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_owned()
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn append(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    let mut s = fs::read_to_string(&p).unwrap_or_default();
    s.push_str(body);
    fs::write(p, s).unwrap();
}

fn replace(root: &Path, rel: &str, from: &str, to: &str) {
    let p = root.join(rel);
    let s = fs::read_to_string(&p).unwrap();
    assert!(s.contains(from), "{rel} does not contain {from:?}");
    fs::write(p, s.replace(from, to)).unwrap();
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!("worktree-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn seq_for(attr: &[worktree::Attribution], needle: &str) -> Option<u64> {
    attr.iter().find(|a| a.text.contains(needle)).map(|a| a.seq)
}

// -------------------------------------------------------------------- tests --

/// §13: the user's own checkout is never an agent's working directory, and a
/// fresh worktree contains none of their uncommitted work.
#[test]
fn a_run_worktree_does_not_contain_the_users_in_flight_work() {
    let f = Fixture::new("isolation");
    let wt = Worktree::create(&f.repo, &RunId::from("run_1"), "run/1", "HEAD").unwrap();

    let auth = fs::read_to_string(wt.path().join("src/auth.ts")).unwrap();
    assert!(
        !auth.contains("scratch note"),
        "the user's staged edit leaked into the run's worktree"
    );
    assert!(
        !wt.path().join("SCRATCH.md").exists(),
        "the user's untracked file leaked into the run's worktree"
    );
    assert_eq!(wt.branch(), "run/1");
    assert!(!wt.is_dirty().unwrap());
}

/// The whole of §13.1, on the cases that defeat matching hunks to `Edit` calls
/// by string comparison.
#[test]
fn attribution_survives_repeat_edits_bash_mutations_and_formatters() {
    let f = Fixture::new("attribution");
    let run = RunId::from("run_1842");
    let wt = Worktree::create(&f.repo, &run, "run/1842", "HEAD").unwrap();
    let w = wt.path().to_path_buf();

    // §13's post-create hook runs before the agent does, and it writes files.
    write(
        &w,
        ".cache/build-info",
        "generated by the post-create hook\n",
    );
    append(&w, "src/auth.ts", "\n// generated: do not edit\n");

    let (mut shadow, root) = ShadowHistory::begin(&wt, &run, 0).unwrap();
    assert_eq!(root.cause.seq, 0);
    assert_eq!(shadow.ref_name(), "refs/agent-workspace/runs/run_1842");

    // seq 4 -- Edit.
    replace(
        &w,
        "src/auth.ts",
        "return fetch(\"/refresh\", { method: \"POST\", body: token });",
        "return retry(() => fetch(\"/refresh\", { method: \"POST\", body: token }), 3);",
    );
    assert!(shadow
        .snapshot(&Cause::tool(4, "Edit", "toolu_01"))
        .unwrap()
        .is_some());

    // seq 7 -- Write.
    write(&w, "src/retry.ts", "export const retry = 1;\n");
    shadow
        .snapshot(&Cause::tool(7, "Write", "toolu_02"))
        .unwrap();

    // seq 9 -- a Read. Most calls are this, and they must not commit.
    assert_eq!(
        shadow
            .snapshot(&Cause::tool(9, "Read", "toolu_03"))
            .unwrap(),
        None,
        "a tool that changed nothing must not commit"
    );

    // seq 11 -- the SAME file edited again.
    replace(
        &w,
        "src/auth.ts",
        "return fetch(\"/logout\");",
        "return retry(() => fetch(\"/logout\"), 2);",
    );
    shadow
        .snapshot(&Cause::tool(11, "Edit", "toolu_04"))
        .unwrap();

    // seq 14 -- a Bash mutation. No tool record says what it touched.
    append(&w, "src/auth.ts", "\nexport const VERSION = \"1.0.0\";\n");
    shadow
        .snapshot(&Cause::tool(14, "Bash", "toolu_05"))
        .unwrap();

    // seq 18 -- a formatter, rewriting a file no tool call named.
    replace(
        &w,
        "src/retry.ts",
        "export const retry = 1;",
        "export const retry = 1; // fmt",
    );
    shadow
        .snapshot(&Cause::tool(18, "Bash", "toolu_06"))
        .unwrap();

    let auth = shadow.attribute("src/auth.ts").unwrap();
    assert_eq!(seq_for(&auth, "fetch(\"/refresh\""), Some(4));
    assert_eq!(
        seq_for(&auth, "fetch(\"/logout\""),
        Some(11),
        "second edit of one file"
    );
    assert_eq!(
        seq_for(&auth, "VERSION = \"1.0.0\""),
        Some(14),
        "Bash mutation"
    );
    assert_eq!(
        seq_for(&auth, "generated: do not edit"),
        Some(0),
        "post-create hook output must not be attributed to a tool call"
    );
    assert_eq!(
        seq_for(
            &shadow.attribute(".cache/build-info").unwrap(),
            "post-create hook"
        ),
        Some(0)
    );
    assert_eq!(
        seq_for(&shadow.attribute("src/retry.ts").unwrap(), "// fmt"),
        Some(18),
        "formatter output in an undeclared file"
    );

    // The tool that produced a line is on the attribution too, not just its seq.
    let bash_line = auth.iter().find(|a| a.text.contains("VERSION")).unwrap();
    assert_eq!(bash_line.tool, "Bash");
    assert_eq!(bash_line.tool_use_id.as_deref(), Some("toolu_05"));
    assert!(bash_line.line > 0);

    // Deletions: blame cannot see them, so Review asks a different question.
    replace(
        &w,
        "src/auth.ts",
        "export function logout() {\n  return retry(() => fetch(\"/logout\"), 2);\n}\n",
        "",
    );
    shadow
        .snapshot(&Cause::tool(21, "Edit", "toolu_07"))
        .unwrap();
    assert_eq!(
        shadow
            .removed_at("src/auth.ts", "fetch(\"/logout\")")
            .unwrap(),
        Some(21)
    );
    assert_eq!(
        seq_for(
            &shadow.attribute("src/auth.ts").unwrap(),
            "fetch(\"/logout\""
        ),
        None,
        "blame cannot answer for a line that no longer exists"
    );

    // §20.5's run replay: the worktree at any seq is a git command.
    let at_root = shadow.tree_at(&root.id, "src/auth.ts").unwrap();
    assert!(at_root.contains("fetch(\"/refresh\", { method: \"POST\""));
    assert!(!at_root.contains("retry("));
}

/// None of this may touch the user's own state.
#[test]
fn snapshotting_never_touches_the_users_head_index_or_worktree() {
    let f = Fixture::new("untouched");
    let run = RunId::from("run_2");
    let (head, tree, mtime) = (f.head(), f.staged_tree(), f.index_mtime());

    let wt = Worktree::create(&f.repo, &run, "run/2", "HEAD").unwrap();
    let (mut shadow, _) = ShadowHistory::begin(&wt, &run, 0).unwrap();
    for i in 1..=5 {
        append(wt.path(), "src/auth.ts", &format!("// {i}\n"));
        shadow.snapshot(&Cause::tool(i, "Edit", "toolu")).unwrap();
    }

    assert_eq!(f.head(), head, "HEAD moved");
    assert_eq!(f.staged_tree(), tree, "the user's staged content changed");
    assert_eq!(f.index_mtime(), mtime, ".git/index was written");
    assert_eq!(
        fs::read_to_string(f.repo.join("SCRATCH.md")).unwrap(),
        "untracked user file\n"
    );
    // .gitignore is respected, so ignored paths stay out of the history.
    let listed = git(
        wt.path(),
        &["ls-tree", "-r", "--name-only", shadow.ref_name()],
    );
    assert!(
        !listed.contains("node_modules"),
        "ignored paths entered the history"
    );
}

/// §13.1's durability claim: Review keeps its attribution after cleanup.
#[test]
fn the_history_outlives_the_worktree_and_a_prune() {
    let f = Fixture::new("outlives");
    let run = RunId::from("run_3");
    let wt = Worktree::create(&f.repo, &run, "run/3", "HEAD").unwrap();
    let (mut shadow, _) = ShadowHistory::begin(&wt, &run, 0).unwrap();
    append(wt.path(), "src/auth.ts", "// edited by the agent\n");
    shadow
        .snapshot(&Cause::tool(4, "Edit", "toolu_01"))
        .unwrap();
    let tip = shadow.tip().unwrap().expect("a tip");

    wt.remove(true).unwrap();
    git(&f.repo, &["branch", "-qD", "run/3"]);
    git(&f.repo, &["reflog", "expire", "--expire=now", "--all"]);
    git(&f.repo, &["gc", "--prune=now", "-q"]);

    assert_eq!(
        git(&f.repo, &["rev-parse", &format!("{tip}^{{commit}}")]),
        tip,
        "the shadow history was garbage-collected away with the worktree"
    );
    let blamed = git(
        &f.repo,
        &["blame", "--line-porcelain", &tip, "--", "src/auth.ts"],
    );
    assert!(blamed.contains("edited by the agent"));
}

/// A run that mutates nothing still has a usable history.
#[test]
fn a_run_that_changes_nothing_has_only_its_root() {
    let f = Fixture::new("noop");
    let run = RunId::from("run_4");
    let wt = Worktree::create(&f.repo, &run, "run/4", "HEAD").unwrap();
    let (mut shadow, root) = ShadowHistory::begin(&wt, &run, 0).unwrap();
    assert_eq!(shadow.snapshot(&Cause::tool(3, "Read", "t")).unwrap(), None);
    assert_eq!(shadow.snapshot(&Cause::tool(5, "Grep", "t")).unwrap(), None);
    assert_eq!(shadow.tip().unwrap().as_deref(), Some(root.id.as_str()));
    assert_eq!(
        git(wt.path(), &["rev-list", "--count", shadow.ref_name()]),
        "1"
    );
}

/// The lookup a fork is built on: the parent's tree as it stood at a `seq`.
///
/// The `<=` rule is the whole of it. §13.1 never commits an unchanged tree, so
/// most `seq` values have no commit of their own and an exact match would answer
/// `None` for almost every block a user can click on — while the tree at those
/// values is, precisely because nothing was committed, the one the last snapshot
/// holds.
#[test]
fn the_commit_at_a_seq_is_the_newest_snapshot_at_or_before_it() {
    let f = Fixture::new("commit-at");
    let run = RunId::from("run_5");
    let wt = Worktree::create(&f.repo, &run, "run/5", "HEAD").unwrap();
    let w = wt.path().to_path_buf();

    // The post-create hook, then the root: `seq` 0 is the tree as the agent
    // found it, hook output included.
    write(&w, ".cache/build-info", "hook\n");
    let (mut shadow, root) = ShadowHistory::begin(&wt, &run, 0).unwrap();

    // A branch point *before the first snapshot* resolves to the root. Not a
    // special case in the implementation and not one here either: 0 <= n for
    // every n, so the root answers for everything until something else commits.
    for seq in [0, 1, 3, 7] {
        assert_eq!(
            shadow.commit_at(seq).unwrap().as_deref(),
            Some(root.id.as_str()),
            "seq {seq} is before the run's first snapshot, so the tree is as the \
             agent found it"
        );
    }

    append(&w, "src/auth.ts", "// eight\n");
    let eight = shadow
        .snapshot(&Cause::tool(8, "Edit", "toolu_01"))
        .unwrap()
        .expect("a changed tree commits");
    append(&w, "src/auth.ts", "// twenty\n");
    let twenty = shadow
        .snapshot(&Cause::tool(20, "Edit", "toolu_02"))
        .unwrap()
        .expect("a changed tree commits");

    // A read between the two writes changed nothing, so it has no commit -- and
    // the answer for its `seq` is the snapshot before it rather than nothing.
    assert_eq!(
        shadow
            .snapshot(&Cause::tool(14, "Read", "toolu_03"))
            .unwrap(),
        None
    );

    assert_eq!(
        shadow.commit_at(7).unwrap().as_deref(),
        Some(root.id.as_str())
    );
    assert_eq!(
        shadow.commit_at(8).unwrap().as_deref(),
        Some(eight.id.as_str())
    );
    for seq in [9, 14, 19] {
        assert_eq!(
            shadow.commit_at(seq).unwrap().as_deref(),
            Some(eight.id.as_str()),
            "seq {seq} has no snapshot of its own, so the tree is the one seq 8 left"
        );
    }
    assert_eq!(
        shadow.commit_at(20).unwrap().as_deref(),
        Some(twenty.id.as_str())
    );
    assert_eq!(
        shadow.commit_at(9_999).unwrap().as_deref(),
        Some(twenty.id.as_str()),
        "past the tip is the tip, not an error"
    );

    // And the commits actually carry the content their `seq` claims, which is
    // what a fork checked out from one of them depends on.
    assert!(!shadow
        .tree_at(&root.id, "src/auth.ts")
        .unwrap()
        .contains("eight"));
    assert!(shadow
        .tree_at(&eight.id, "src/auth.ts")
        .unwrap()
        .contains("eight"));
    assert!(!shadow
        .tree_at(&eight.id, "src/auth.ts")
        .unwrap()
        .contains("twenty"));
    assert!(shadow
        .tree_at(&twenty.id, "src/auth.ts")
        .unwrap()
        .contains("twenty"));
}

/// §13.1: a run whose shadow ref is gone reports attribution as *unavailable*,
/// which is a different answer from "this file has nothing attributed".
///
/// The distinction is the test. A Review that reads an empty result as "no
/// attribution here" will look correct on every run anyone tries it on, because
/// the runs anyone tries it on still have their refs.
#[test]
fn a_run_whose_shadow_ref_is_gone_is_unavailable_not_empty() {
    let f = Fixture::new("unavailable");
    let run = RunId::from("run_6");
    let wt = Worktree::create(&f.repo, &run, "run/6", "HEAD").unwrap();
    let (mut shadow, _) = ShadowHistory::begin(&wt, &run, 0).unwrap();
    append(wt.path(), "src/auth.ts", "// edited by the agent\n");
    shadow
        .snapshot(&Cause::tool(4, "Edit", "toolu_01"))
        .unwrap();

    assert!(shadow.is_available().unwrap());
    assert!(!shadow.attribute("src/auth.ts").unwrap().is_empty());
    assert_eq!(shadow.commit_at(4).unwrap(), shadow.tip().unwrap());

    // The run is cleaned up: worktree gone, ref gone. The row in the log still
    // points at both.
    let ref_name = shadow.ref_name().to_owned();
    git(&f.repo, &["update-ref", "-d", &ref_name]);

    let orphan = ShadowHistory::open(&wt, &run).unwrap();
    assert!(
        !orphan.is_available().unwrap(),
        "a missing ref is the state Review has to report, so it has to be askable"
    );
    assert_eq!(
        orphan.commit_at(4).unwrap(),
        None,
        "there is no tree to fork from either, and that is not `seq` 0"
    );
    assert!(
        orphan.attribute("src/auth.ts").is_err(),
        "attribution against a ref that is gone must fail rather than come back \
         empty, which would read as `nothing here was written by this run`"
    );
    assert!(orphan.removed_at("src/auth.ts", "agent").is_err());
}
