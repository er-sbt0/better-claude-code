#!/usr/bin/env bash
#
# Does a plain (non-forking) resume tolerate `--session-id X --resume X`?
#
# The driver emitted exactly that: `LaunchSpec::argv` pushed `--session-id`
# unconditionally and `driver::resume` set `resume` to the same id. Nothing had
# ever checked it against a real CLI — `claude-code-session-forking.md` verified
# `--session-id` on a *fresh* session and `--session-id` with `--fork-session`,
# never `--session-id` naming a session that already exists on a resume that is
# not a fork. §16's rule is that help text is not evidence, and the shape was
# never in any recorded fixture either.
#
# The argv below is `LaunchSpec::argv` reproduced faithfully, prompt on stdin as
# a stream-json `user` message, minus the workspace MCP flags (no server here)
# and bounded at one turn. Asserts rather than prints: exits non-zero on any
# regression. Six live runs, well under a cent on haiku.
#
#   bash docs/verification/resume-session-id-spike.sh
#
# Env: CLAUDE_BIN (default: claude), MODEL (default: claude-haiku-4-5-20251001)

set -uo pipefail

CLAUDE_BIN="${CLAUDE_BIN:-claude}"
MODEL="${MODEL:-claude-haiku-4-5-20251001}"
WORK="$(mktemp -d -t aw-resume-XXXXXX)"
SANDBOX="$WORK/sandbox"

fail=0
ok()   { printf '  ok       %s\n' "$1"; }
bad()  { printf '  FAIL     %s\n' "$1"; fail=1; }
check(){ if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (want '$3', got '$2')"; fi; }
contains(){ case "$2" in *"$3"*) ok "$1";; *) bad "$1 (want '$3' in '$2')";; esac }

command -v "$CLAUDE_BIN" >/dev/null || { echo "no claude on PATH" >&2; exit 1; }
echo "claude: $($CLAUDE_BIN --version)"
echo "work:   $WORK"

mkdir -p "$SANDBOX"
git -C "$SANDBOX" init -q -b main
git -C "$SANDBOX" config user.email "fixtures@example.com"
git -C "$SANDBOX" config user.name "Fixture Recorder"
echo "placeholder" > "$SANDBOX/README.md"
git -C "$SANDBOX" add -A
git -C "$SANDBOX" commit -qm "init"

uuid() { python3 -c 'import uuid;print(uuid.uuid4())'; }

# run <label> <prompt> [extra claude args...]
# Sets EXIT, SUBTYPE, SID, INIT_SID, REPLY, ERRTXT.
run() {
  local label="$1" prompt="$2"; shift 2
  python3 -c 'import json,sys;print(json.dumps({"type":"user","message":{"role":"user","content":[{"type":"text","text":sys.argv[1]}]},"parent_tool_use_id":None}))' "$prompt" \
  | ( cd "$SANDBOX" && timeout 120 "$CLAUDE_BIN" \
      --output-format stream-json --verbose \
      --input-format stream-json --replay-user-messages \
      --strict-mcp-config --thinking-display summarized \
      --model "$MODEL" --permission-prompts none \
      --max-turns 1 --max-budget-usd 0.05 \
      "$@" -p ) > "$WORK/$label.ndjson" 2> "$WORK/$label.stderr"
  EXIT=$?
  SUBTYPE="$(jq -r 'select(.type=="result") | .subtype // empty' "$WORK/$label.ndjson" 2>/dev/null | head -1)"
  SID="$(jq -r 'select(.type=="result") | .session_id // empty' "$WORK/$label.ndjson" 2>/dev/null | head -1)"
  INIT_SID="$(jq -r 'select(.type=="system" and .subtype=="init") | .session_id // empty' "$WORK/$label.ndjson" 2>/dev/null | head -1)"
  REPLY="$(jq -r 'select(.type=="result") | .result // ""' "$WORK/$label.ndjson" 2>/dev/null | tr -d '\n')"
  ERRTXT="$(tr -d '\n' < "$WORK/$label.stderr")"
  printf '  [%s] exit=%s subtype=%s session=%s reply=%s\n' \
    "$label" "$EXIT" "${SUBTYPE:-NONE}" "${SID:-none}" "${REPLY:0:40}"
}

T1="$(uuid)"; T2="$(uuid)"; T3="$(uuid)"
echo
echo "three trunks, each under a session id we assign"
run trunk1 "Remember the codeword ZEPHYR. Reply with the single word ok" --session-id "$T1"
check "a fresh --session-id is ours to assign" "$SID" "$T1"
run trunk2 "Remember the codeword QUASAR. Reply with the single word ok" --session-id "$T2"
run trunk3 "Remember the codeword VERTEX. Reply with the single word ok" --session-id "$T3"

echo
echo "the argv the driver built: --session-id X --resume X"
run plain "What codeword did I give you? Reply with just the codeword." --session-id "$T1" --resume "$T1"
check "refused: no process to speak of" "$EXIT" "1"
check "and nothing on stdout -- no result line to read a subtype off" "${SUBTYPE:-NONE}" "NONE"
contains "the refusal names the rule" "$ERRTXT" \
  "--session-id can only be used with --continue or --resume if --fork-session is also specified"

echo
echo "the fix: --resume X alone"
run fixed "What codeword did I give you? Reply with just the codeword." --resume "$T2"
check "accepted" "$SUBTYPE" "success"
check "and it keeps the id it had -- nothing was lost by dropping the flag" "$SID" "$T2"
check "init agrees" "$INIT_SID" "$T2"
contains "the transcript came with it" "$REPLY" "QUASAR"

echo
echo "is the refusal about the flag or about the combination?"
run reused "What codeword did I give you? Reply with just the codeword." --session-id "$T3"
check "a --session-id naming an existing session is refused on a fresh launch too" "$EXIT" "1"
contains "but for a different reason: uniqueness, not composition" "$ERRTXT" "is already in use"

echo
if [ "$fail" = 0 ]; then echo "all assertions held"; else echo "REGRESSION"; fi
echo "raw streams: $WORK"
exit "$fail"
