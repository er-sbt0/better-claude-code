<!--
Verification record. Discharges the "Review attribution mechanism is asserted in
§1, §7 and §10 and specified nowhere" debt in HANDOFF.md, and settles the
question the handoff said had to be answered before the process driver exists.

Verified: 2026-09-12 · git 2.48.1 · Ubuntu, x86-64
Method: an executable spike, docs/verification/shadow-attribution-spike.sh,
        which asserts rather than prints and exits non-zero on regression.
        Timings from a separate synthetic-repo harness, medians of repeats.
        No agent runs were used to produce this document.
-->

# Review attribution — shadow-commit snapshots

`agent-workspace-design.md` promises, in §1, §7 and §10, that Review shows "each
hunk next to the reasoning and tool calls that produced it". Nothing specified
how a hunk is matched to a tool call. This record establishes the mechanism and
what it costs.

## Why the obvious approach does not work

Matching hunks to `Edit` calls by comparing `old_string` / `new_string` against
the diff fails on the cases that Review exists to explain:

| Case | What breaks |
|---|---|
| A file edited three times in one run | Three candidate calls, one hunk; string match picks by luck |
| `Write` followed by `Edit` | The `Write` content no longer exists to match against |
| A formatter, a codemod, `npm install` | Mutates files no tool call named |
| A `Bash` mutation (`sed -i`, `>>`, `mv`) | No tool record of file or content at all |
| A tool call that fails halfway | Declared intent, partial outcome |

Every one of these has the same shape: the tool call records an *intention*, and
Review needs the *outcome*. So record outcomes.

## The mechanism

After every mutating tool call, snapshot the run's worktree into a commit on a
private ref, using a separate index file so the user's own index is never
touched:

```sh
parent=$(git rev-parse --verify -q "$REF" || true)
tree=$(GIT_INDEX_FILE=$SHADOW_INDEX sh -c 'git add -A && git write-tree')
commit=$(git commit-tree "$tree" ${parent:+-p "$parent"} \
           -m "seq $seq $tool" \
           -m "$(printf 'run: %s\nseq: %s\ntool: %s\ntool-use-id: %s' ...)")
git update-ref "$REF" "$commit"
```

`REF` is `refs/agent-workspace/runs/<run_id>`; `SHADOW_INDEX` lives in the
worktree's git dir. Attribution is then `git blame` against that synthetic
history: hunk line → shadow commit → `seq` trailer → the events at that `seq`.

## What the spike proves

`bash docs/verification/shadow-attribution-spike.sh` — 15 assertions, all
passing at git 2.48.1. `crates/worktree` implements the mechanism and carries
the same assertions into CI, so a git upgrade that changes this breaks a test
rather than Review:

**The user's own state is untouched.** HEAD unmoved, staged content
byte-identical, `.git/index` never written (mtime unchanged), `.gitignore`d
paths absent from the shadow tree.

**Attribution is correct on exactly the cases that defeat string matching.** A
file's first edit attributes to its `seq`; a second edit of the same file
attributes to *its* `seq`, not the first; a `Bash` append with no tool record of
its content attributes correctly; a formatter rewriting a file no tool call
named attributes to the `seq` that ran it.

**Deleted lines are attributable too**, though not by blame — see the limits
below.

**Review's diff is fully covered.** The worktree tree and the shadow tip tree
are identical at run end, so every hunk Review can show has a shadow commit
behind it.

**The history outlives the run.** After `git worktree remove --force`, deleting
the run branch, and `git gc --prune=now`, the shadow history is still complete
and blame still answers. The ref is in the shared ref namespace, so it holds the
objects on its own.

## What the spike changed about the design

Five things, each of which would have been a defect found later:

1. **The shadow ref must be rooted at a run-start snapshot, not at the base
   commit.** §13 allows a post-create hook (dependency install, codegen) that
   runs before the agent does and writes files. Rooted at the base commit, all
   of it is attributed to the first tool call. A `seq 0` snapshot taken after
   the hook and before the first call gives it somewhere else to land.
2. **Worktree-per-run already removes the larger hazard.** The user's staged and
   untracked work is not in a fresh worktree at all, so it cannot be swept into
   a run's history even in principle. This is a second, unadvertised payoff of
   §13 — asserted in the spike so a future change to worktree setup breaks a
   test rather than the attribution.
3. **A snapshot whose tree is unchanged must not commit.** Read-only and failed
   tool calls are most calls. `ShadowHistory::snapshot` returns `None` for them;
   empty commits in the history make blame attribute to the wrong `seq`.
4. **All trailers must be in one `-m` paragraph.** git parses only the last
   paragraph of a message as trailers, so `-m "seq: 4" -m "tool: Edit"` yields
   *silently empty* `%(trailers:key=seq)` — the first version of the spike read
   back nothing but looked like it worked.
5. **Seed the shadow index from the real one** (`cp .git/index $SHADOW_INDEX`).
   Otherwise the first snapshot re-hashes every file in the repo.

## Cost

Synthetic repos, 60-line TypeScript files, medians of repeats, shadow index
seeded (ms):

| Repo | First snapshot | 1-file edit | No-op call | 200-file format | Blame one file |
|---|---|---|---|---|---|
| 500 files | 14 | 27 | 21 | 14 | 5 |
| 5,000 files | 84 | 39 | 25 | 19 | 4 |
| 20,000 files | 243 | 26 | 33 | 32 | 4 |

Incremental snapshots are ~25–40 ms and near-flat in repo size: the cost is the
worktree scan, not hashing or committing. Only the first snapshot scales, and it
happens once per run. Object cost is small — git dedupes unchanged blobs and
trees, and the spike's seven-snapshot run packs to 5.22 KiB.

Sub-40 ms per mutating tool call, against tool calls that themselves take
hundreds of milliseconds to seconds, is affordable.

## Limits, and what is not proven

- **Deleted lines need the pickaxe, not blame.** Blame only sees lines that
  survived to the tip. For a removed hunk, the answer is the newest shadow
  commit whose diff removed it — `git log -n 1 -S <text> $REF -- <path>`
  (asserted in the spike). Review has to run a second query for deletions; a
  naive implementation will silently show no attribution for them.
- **Parallel tool calls degrade granularity.** The snapshot is taken after a
  tool completes, so concurrent calls — subagents, per `parent_tool_use_id` —
  can land in one snapshot, and attribution is then "one of these calls". Not
  reproduced here: every recorded fixture has `parent_tool_use_id` null
  (HANDOFF.md), so this is untested against real subagent traffic. Snapshots
  must in any case be serialized per worktree; the shadow index is not
  concurrency-safe.
- **`git add -A` sweeps untracked build output that is not `.gitignore`d.** A
  test run emitting thousands of untracked files enters the shadow history. The
  worktree's `$GIT_DIR/info/exclude` is the place to add run-scoped excludes.
- **Not exercised:** submodules, symlink and mode changes, large binaries,
  `blame -M/-C` for moved code, a run that itself commits, rebases or checks out
  inside its worktree, and any non-Linux platform.
