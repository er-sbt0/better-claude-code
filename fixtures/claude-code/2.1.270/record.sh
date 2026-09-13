#!/usr/bin/env bash
# Record golden `stream-json` fixtures from Claude Code (agent-workspace-design.md §16).
#
#   ./record.sh            # record every scenario
#   ./record.sh tool-edit  # record only the named scenario(s)
#
# Every nested `claude` run happens in a throwaway git sandbox, NEVER in this
# repository. Raw (unredacted) captures stay in a scratch dir; only redacted
# copies are written next to this script.
#
# Env overrides:
#   CLAUDE_BIN    claude executable            (default: claude on PATH)
#   SANDBOX_DIR   sandbox git repo location    (default: $WORK/sandbox)
#   RAW_DIR       raw capture location         (default: $WORK/raw)
#   MODEL         model alias                  (default: sonnet)
#   RUN_TIMEOUT   per-run wall clock, seconds  (default: 300)

set -uo pipefail

OUT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CLAUDE_BIN="${CLAUDE_BIN:-claude}"
MODEL="${MODEL:-sonnet}"
RUN_TIMEOUT="${RUN_TIMEOUT:-300}"
WORK="${WORK:-$(mktemp -d -t cc-fixtures-XXXXXX)}"
SANDBOX_DIR="${SANDBOX_DIR:-$WORK/sandbox}"
RAW_DIR="${RAW_DIR:-$WORK/raw}"

command -v "$CLAUDE_BIN" >/dev/null || { echo "no claude on PATH" >&2; exit 1; }
VERSION="$("$CLAUDE_BIN" --version)"
echo "claude: $VERSION"
echo "sandbox: $SANDBOX_DIR"
echo "raw:     $RAW_DIR"
echo "out:     $OUT_DIR"

mkdir -p "$RAW_DIR"

# setup_sandbox() does `rm -rf "$SANDBOX_DIR"`, and SANDBOX_DIR is overridable,
# so refuse anything that would delete something we care about. The repo check
# is against the whole worktree, not just this directory.
REPO_ROOT="$(git -C "$OUT_DIR" rev-parse --show-toplevel 2>/dev/null || echo "$OUT_DIR")"
case "$SANDBOX_DIR" in
  ""|"/"|"$HOME"|"$HOME"/) echo "refusing: unsafe SANDBOX_DIR '$SANDBOX_DIR'" >&2; exit 1;;
  "$REPO_ROOT"|"$REPO_ROOT"/*) echo "refusing: sandbox inside the design repo" >&2; exit 1;;
esac
case "$SANDBOX_DIR" in
  /*) ;;
  *) echo "refusing: SANDBOX_DIR must be an absolute path" >&2; exit 1;;
esac

# ---------------------------------------------------------------- sandbox ----
setup_sandbox() {
  rm -rf "$SANDBOX_DIR"
  mkdir -p "$SANDBOX_DIR/src"
  cat > "$SANDBOX_DIR/README.md" <<'EOF'
# calc-demo

A tiny calculator used for fixture recording.

- `src/calc.ts` — arithmetic helpers
- `src/stats.ts` — average helper
EOF
  cat > "$SANDBOX_DIR/package.json" <<'EOF'
{
  "name": "calc-demo",
  "version": "0.1.0",
  "private": true,
  "type": "module",
  "scripts": {
    "test": "echo \"no tests\" && exit 0"
  }
}
EOF
  # subtract() has the obvious bug: it adds.
  cat > "$SANDBOX_DIR/src/calc.ts" <<'EOF'
export function add(a: number, b: number): number {
  return a + b;
}

export function subtract(a: number, b: number): number {
  return a + b;
}

export function multiply(a: number, b: number): number {
  return a * b;
}
EOF
  cat > "$SANDBOX_DIR/src/stats.ts" <<'EOF'
export function average(xs: number[]): number {
  let total = 0;
  for (const x of xs) {
    total += x;
  }
  return total / xs.length;
}
EOF
  git -C "$SANDBOX_DIR" init -q -b main
  git -C "$SANDBOX_DIR" config user.email "fixtures@example.com"
  git -C "$SANDBOX_DIR" config user.name "Fixture Recorder"
  git -C "$SANDBOX_DIR" add -A
  git -C "$SANDBOX_DIR" commit -qm "init: calc demo"
}

# Flags shared by every run. --strict-mcp-config with no --mcp-config keeps
# ambient user MCP servers out of the init event.
common_flags() {
  printf '%s\n' \
    --output-format stream-json --verbose \
    --strict-mcp-config \
    --model "$MODEL" \
    --max-budget-usd 0.50
}

# run <name> <claude args...>   (stdin is /dev/null unless redirected)
run() {
  local name="$1"; shift
  echo "  -> $name"
  local flags; mapfile -t flags < <(common_flags)
  ( cd "$SANDBOX_DIR" && timeout "$RUN_TIMEOUT" "$CLAUDE_BIN" "${flags[@]}" "$@" ) \
    > "$RAW_DIR/$name.ndjson" 2> "$RAW_DIR/$name.stderr"
  local rc=$?
  echo "     exit=$rc lines=$(wc -l < "$RAW_DIR/$name.ndjson") bytes=$(wc -c < "$RAW_DIR/$name.ndjson")"
  [ -s "$RAW_DIR/$name.stderr" ] && echo "     stderr: $(head -c 300 "$RAW_DIR/$name.stderr")"
  return 0
}

# ------------------------------------------------------------- scenarios ----
sc_text_only() {
  run text-only -p "what is 2+2, answer in one word" --max-turns 3
}

sc_tool_read() {
  run tool-read -p "Read src/stats.ts and describe in one sentence what it does. Do not edit anything." \
    --max-turns 4 --allowed-tools "Read"
}

sc_tool_edit() {
  run tool-edit -p "src/calc.ts has one obvious bug. Fix it with a single Edit, then stop. Do not run any commands." \
    --max-turns 6 --permission-mode acceptEdits --allowed-tools "Read" "Edit"
}

sc_tool_bash() {
  run tool-bash -p "Run the command 'git status --short --branch' and tell me its output. Use only that one command." \
    --max-turns 4 --allowed-tools "Bash(git status:*)"
}

# Extra scenario (not in the original target set): does a NON-ZERO exit code
# reach the wire? §6 "source 1" claims exit codes come for free.
sc_tool_bash_fail() {
  run tool-bash-fail -p "Run the command 'git cat-file -e deadbeefdeadbeefdeadbeefdeadbeefdeadbeef' and report its exit status. Use only that one command." \
    --max-turns 4 --allowed-tools "Bash(git cat-file:*)"
}

# NB: --thinking-display summarized is REQUIRED. Without it, print mode still
# emits `thinking` blocks but their `thinking` string is EMPTY (signature only).
THINK_PROMPT="Without using any tools, work this out and reply with just the number: what is the smallest positive integer n such that n! ends in exactly 100 trailing zeros?"

sc_thinking() {
  run thinking -p "$THINK_PROMPT" \
    --max-turns 3 --thinking enabled --effort high --thinking-display summarized
}

# Extra scenario (not in the original target set): thinking + partial messages,
# the only place `thinking_delta` / `signature_delta` appear on the wire.
sc_thinking_partial() {
  run thinking-partial -p "$THINK_PROMPT" \
    --max-turns 3 --thinking enabled --effort high --thinking-display summarized \
    --include-partial-messages
}

sc_partial_messages() {
  run partial-messages -p "what is 2+2, answer in one word" \
    --max-turns 3 --include-partial-messages
}

# Two user turns written to stdin of ONE process. The second turn is only sent
# after the first turn's `result` message is seen, so the transcript is ordered.
sc_multi_turn() {
  echo "  -> multi-turn"
  local drv="$WORK/multiturn.py"
  cat > "$drv" <<'PY'
import json, subprocess, sys, threading

claude, sandbox, out, model, timeout_s = sys.argv[1:6]
turns = ["Say the word HELLO and nothing else.",
         "Now say it backwards, and nothing else."]

argv = [claude, "--output-format", "stream-json", "--verbose",
        "--strict-mcp-config", "--model", model, "--max-budget-usd", "0.50",
        "--input-format", "stream-json", "--replay-user-messages",
        "--max-turns", "6", "-p"]

p = subprocess.Popen(argv, cwd=sandbox, stdin=subprocess.PIPE,
                     stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

def send(text):
    p.stdin.write(json.dumps({
        "type": "user",
        "message": {"role": "user", "content": [{"type": "text", "text": text}]},
        "parent_tool_use_id": None,
    }) + "\n")
    p.stdin.flush()

def killer():
    try:
        p.wait(timeout=float(timeout_s))
    except subprocess.TimeoutExpired:
        p.kill()
threading.Thread(target=killer, daemon=True).start()

send(turns[0])
results = 0
with open(out, "w") as fh:
    for line in p.stdout:
        fh.write(line)
        fh.flush()
        try:
            msg = json.loads(line)
        except Exception:
            continue
        if msg.get("type") == "result":
            results += 1
            if results == 1:
                send(turns[1])
            else:
                break
try:
    p.stdin.close()
except Exception:
    pass
p.terminate()
sys.stderr.write(p.stderr.read() or "")
PY
  python3 "$drv" "$CLAUDE_BIN" "$SANDBOX_DIR" "$RAW_DIR/multi-turn.ndjson" "$MODEL" "$RUN_TIMEOUT" \
    2> "$RAW_DIR/multi-turn.stderr"
  echo "     lines=$(wc -l < "$RAW_DIR/multi-turn.ndjson") bytes=$(wc -c < "$RAW_DIR/multi-turn.ndjson")"
}

sc_resume() {
  local uuid="${FIXTURE_SESSION_ID:-$(python3 -c 'import uuid;print(uuid.uuid4())')}"
  echo "  (session-id $uuid)"
  run resume-1 -p "Remember the number 42. Reply with just: OK" \
    --max-turns 3 --session-id "$uuid"
  run resume-2 -p "What number did I ask you to remember? Reply with just the number." \
    --max-turns 3 --resume "$uuid"
}

sc_permission_denied() {
  # Write is not pre-allowed and nobody can answer the prompt => auto-denied.
  # (NB: a Bash "echo" is auto-allowed by Claude Code's own safe-command logic,
  # so it does NOT produce a denial -- use a file write instead.)
  run permission-denied -p "Create a file notes.txt in the repo root containing the single word hello." \
    --max-turns 4 --permission-prompts none
}

sc_error() {
  # A task that cannot finish in one turn => result subtype error_max_turns.
  run error -p "Read src/does-not-exist.ts and summarise it." \
    --max-turns 1 --allowed-tools "Read"
}

# The workspace MCP server (design §6 source 3, §12 option A). New in 2.1.270.
#
# Unlike every other scenario this one needs a second process alive for the
# duration: our server, which `claude` reaches through a relay it spawns
# itself. `examples/host` binds the socket and writes the flags that implies to
# a file; we source them and run `claude` exactly as usual, so what lands in the
# fixture is still nothing but the provider's stdout.
#
# What it is evidence of: that a workspace tool call arrives as an ordinary
# assistant(tool_use)/user(tool_result) pair under the `mcp__agent-workspace__`
# prefix. It is NOT evidence about `permission.requested` -- that never appears
# on stdout at all, because the provider's permission layer calls our tool
# directly rather than the model calling it. No fixture can ever contain one.
WORKSPACE_MCP_PROMPT="src/calc.ts has one obvious bug in subtract(). Fix it with a single Edit, then call the report_finding tool to record what the bug was. Do not run any commands."

sc_workspace_mcp() {
  echo "  -> workspace-mcp"
  local repo_root host relay flags
  repo_root="$(git -C "$OUT_DIR" rev-parse --show-toplevel)"
  host="$repo_root/target/debug/examples/host"
  relay="$repo_root/target/debug/workspace-mcp-relay"
  for b in "$host" "$relay"; do
    [ -x "$b" ] || { echo "     missing $b -- run: cargo build -p workspace-mcp --examples --bins" >&2; return 0; }
  done

  flags="$WORK/workspace-mcp.flags"
  rm -f "$flags"
  # Hold the host's stdin open for the life of the run: it serves until stdin
  # closes, which is how it gets to unlink its own socket.
  local fifo="$WORK/host.stdin"
  rm -f "$fifo"; mkfifo "$fifo"
  "$host" --relay "$relay" --out "$flags" --allow < "$fifo" 2> "$RAW_DIR/workspace-mcp.host" &
  local host_pid=$!
  exec {hold}>"$fifo"

  for _ in $(seq 1 50); do [ -s "$flags" ] && break; sleep 0.1; done
  if [ ! -s "$flags" ]; then
    echo "     host never wrote its flags; see $RAW_DIR/workspace-mcp.host" >&2
    exec {hold}>&-; wait "$host_pid" 2>/dev/null; return 0
  fi
  # shellcheck disable=SC1090
  . "$flags"

  run workspace-mcp -p "$WORKSPACE_MCP_PROMPT" \
    --max-turns 6 \
    --mcp-config "$MCP_CONFIG" \
    --permission-prompt-tool "$PERMISSION_TOOL" \
    --allowed-tools "Read" "$REPORT_TOOL" \
    --append-system-prompt "$SYSTEM_PROMPT" --system-prompt-snapshot off

  exec {hold}>&-
  wait "$host_pid"; local hrc=$?
  [ "$hrc" -eq 0 ] || echo "     host exited $hrc (3 = nothing ever asked for permission)" >&2
}

# ------------------------------------------------------------- redaction ----
redact() {
  python3 - "$RAW_DIR" "$OUT_DIR" "$SANDBOX_DIR" <<'PY'
import os, re, sys

raw_dir, out_dir, sandbox = sys.argv[1:4]
home = os.path.expanduser("~")
mangled_sandbox = sandbox.replace("/", "-")
mangled_home = home.replace("/", "-")

subs = [
    (sandbox, "/FIXTURE_SANDBOX"),
    (mangled_sandbox, "-FIXTURE_SANDBOX"),
    (home, "/home/USER"),
    (mangled_home, "-home-USER"),
]

# Credential-shaped things we never want in git.
scans = [
    ("anthropic-key", re.compile(r"sk-[A-Za-z0-9_\-]{16,}")),
    ("bearer", re.compile(r"(?i)bearer\s+[A-Za-z0-9._\-]{16,}")),
    ("authheader", re.compile(r"(?i)x-api-key\"?\s*[:=]\s*\"?[A-Za-z0-9._\-]{16,}")),
    ("email", re.compile(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}")),
    ("ghtoken", re.compile(r"gh[pousr]_[A-Za-z0-9]{20,}")),
]
allow_emails = {"fixtures@example.com"}

os.makedirs(out_dir, exist_ok=True)
found = []
for name in sorted(os.listdir(raw_dir)):
    if not name.endswith(".ndjson"):
        continue
    text = open(os.path.join(raw_dir, name), encoding="utf-8", errors="surrogateescape").read()
    for needle, token in subs:
        text = text.replace(needle, token)
    for label, rx in scans:
        for m in rx.findall(text):
            hit = m if isinstance(m, str) else m[0]
            if label == "email" and hit in allow_emails:
                continue
            found.append((name, label, hit))
            text = text.replace(hit, "REDACTED_" + label.upper())
    with open(os.path.join(out_dir, name), "w", encoding="utf-8", errors="surrogateescape") as fh:
        fh.write(text)
    print(f"  redacted {name} -> {len(text)} bytes")

if found:
    print("  CREDENTIAL-SHAPED MATCHES REPLACED:")
    for f in found:
        print("   ", f)
else:
    print("  no credential-shaped matches found")
PY
}

# ------------------------------------------------------------------ main ----
ALL=(text-only tool-read tool-edit tool-bash tool-bash-fail thinking thinking-partial partial-messages multi-turn resume permission-denied error workspace-mcp)
WANT=("$@")
[ ${#WANT[@]} -eq 0 ] && WANT=("${ALL[@]}")

setup_sandbox
for s in "${WANT[@]}"; do
  case "$s" in
    text-only)         sc_text_only ;;
    tool-read)         sc_tool_read ;;
    tool-edit)         sc_tool_edit ;;
    tool-bash)         sc_tool_bash ;;
    tool-bash-fail)    sc_tool_bash_fail ;;
    thinking)          sc_thinking ;;
    thinking-partial)  sc_thinking_partial ;;
    partial-messages)  sc_partial_messages ;;
    multi-turn)        sc_multi_turn ;;
    resume)            sc_resume ;;
    permission-denied) sc_permission_denied ;;
    error)             sc_error ;;
    workspace-mcp)     sc_workspace_mcp ;;
    *) echo "unknown scenario: $s" >&2; exit 1 ;;
  esac
  # tool-edit mutates the sandbox; reset so later runs see a clean tree.
  git -C "$SANDBOX_DIR" checkout -q -- . 2>/dev/null
  git -C "$SANDBOX_DIR" clean -qfd 2>/dev/null
done

echo "redacting..."
redact
echo "done. raw captures kept in $RAW_DIR"
