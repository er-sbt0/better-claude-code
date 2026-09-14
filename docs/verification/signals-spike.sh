#!/usr/bin/env bash
#
# What does Claude Code do when it is signalled?
#
# `Session::interrupt` sends one signal and it is the last word on two things
# at once: whether the provider gets to flush the session transcript that
# `--resume` depends on, and what `Registry::finish` reads off the exit status
# to decide whether the run was interrupted, finished or failed. Neither had
# been measured -- the driver sent `SIGKILL` because `Child::kill` does.
#
# Four live runs, a trivial prompt each, haiku, well under a cent. Asserts
# rather than prints: exits non-zero on any regression.
#
#   bash docs/verification/signals-spike.sh
#
# Env: CLAUDE_BIN (default: claude), MODEL (default: claude-haiku-4-5-20251001)

set -uo pipefail

CLAUDE_BIN="${CLAUDE_BIN:-claude}"
MODEL="${MODEL:-claude-haiku-4-5-20251001}"
WORK="$(mktemp -d -t aw-signals-XXXXXX)"
SANDBOX="$WORK/sandbox"

fail=0
ok()    { printf '  ok       %s\n' "$1"; }
bad()   { printf '  FAIL     %s\n' "$1"; fail=1; }
check() { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (want '$3', got '$2')"; fi }
under() { if [ "$(echo "$2 < $3" | bc)" = 1 ]; then ok "$1 (${2}s)"; else bad "$1 (${2}s, budget ${3}s)"; fi }
has()   { case "$2" in *"$3"*) ok "$1";; *) bad "$1 (want '$3' in '$2')";; esac }

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
msg()  { python3 -c 'import json,sys;print(json.dumps({"type":"user","message":{"role":"user","content":[{"type":"text","text":sys.argv[1]}]},"parent_tool_use_id":None}))' "$1"; }

# signalled <label> <signal> <codeword>
#
# Starts a run that holds stdin open -- the driver's contract, and the state
# every run is in at app exit -- waits for its turn to finish, signals the
# process *group*, and reports how it left. `setsid` is the shell's spelling of
# `CommandExt::process_group(0)`, which is what the driver now does.
#
# Sets SID, STATUS (bash's 128+n for a signalled exit), SECONDS_TO_EXIT.
signalled() {
  local label="$1" signal="$2" codeword="$3"
  SID="$(uuid)"
  mkfifo "$WORK/$label.in"
  ( cd "$SANDBOX" && setsid "$CLAUDE_BIN" \
      --output-format stream-json --verbose \
      --input-format stream-json --replay-user-messages \
      --strict-mcp-config --thinking-display summarized \
      --model "$MODEL" --permission-prompts none \
      --max-turns 4 --max-budget-usd 0.05 \
      --session-id "$SID" -p \
      < "$WORK/$label.in" > "$WORK/$label.ndjson" 2> "$WORK/$label.stderr"
    echo "$?" > "$WORK/$label.status" ) &
  local shell_pid=$!
  exec {fd}> "$WORK/$label.in"
  msg "Remember the codeword $codeword. Reply with the single word ok." >&$fd

  local n=0
  while [ $n -lt 900 ] && ! grep -q '"type":"result"' "$WORK/$label.ndjson" 2>/dev/null; do
    sleep 0.1; n=$((n+1))
  done
  local pid; pid="$(pgrep -f -- "--session-id $SID" | head -1)"
  local t0 t1; t0=$(date +%s.%N)
  kill -"$signal" "-$pid" 2>/dev/null
  wait "$shell_pid" 2>/dev/null
  t1=$(date +%s.%N)
  STATUS="$(cat "$WORK/$label.status" 2>/dev/null || echo none)"
  SECONDS_TO_EXIT="$(echo "$t1 - $t0" | bc)"
  exec {fd}>&-
  printf '  [%s] %s -> status %s after %.2fs\n' "$label" "$signal" "$STATUS" "$SECONDS_TO_EXIT"
}

# resume <sid> -- one turn against the transcript the signalled run left behind
resume() {
  msg "What codeword did I give you? Reply with just the codeword, or NONE." \
  | ( cd "$SANDBOX" && timeout 120 "$CLAUDE_BIN" \
      --output-format stream-json --verbose --input-format stream-json \
      --strict-mcp-config --model "$MODEL" --permission-prompts none \
      --max-turns 1 --max-budget-usd 0.05 --resume "$1" -p ) \
      > "$WORK/resume-$1.ndjson" 2> "$WORK/resume-$1.stderr"
  RESUME_EXIT=$?
  RESUME_SUBTYPE="$(jq -r 'select(.type=="result")|.subtype//empty' "$WORK/resume-$1.ndjson" 2>/dev/null | head -1)"
  RESUME_REPLY="$(jq -r 'select(.type=="result")|.result//""' "$WORK/resume-$1.ndjson" 2>/dev/null | tr -d '\n')"
  printf '  [resume] exit=%s subtype=%s reply=%s\n' "$RESUME_EXIT" "${RESUME_SUBTYPE:-NONE}" "${RESUME_REPLY:0:40}"
}

echo
echo "SIGTERM -- the signal the driver now sends first"
signalled term TERM ZEPHYR
check "it does not die instantly: it handles the signal" \
      "$(echo "$SECONDS_TO_EXIT > 0.1" | bc)" "1"
under "and it is bounded well inside the driver's grace period" "$SECONDS_TO_EXIT" 2
check "it re-raises rather than exiting: still 'killed by signal 15'" "$STATUS" "143"
resume "$SID"
check "the transcript it left is resumable" "$RESUME_SUBTYPE" "success"
has   "and it is the same conversation" "$RESUME_REPLY" "ZEPHYR"

echo
echo "SIGKILL -- what the driver used to send, and its last resort"
signalled kill KILL QUASAR
under "instant, by construction" "$SECONDS_TO_EXIT" 0.5
check "killed by signal 9" "$STATUS" "137"
resume "$SID"
check "the transcript survives this too -- it is written per message" "$RESUME_SUBTYPE" "success"

echo
echo "SIGINT -- the trap"
signalled int INT VERTEX
check "it is handled, like SIGTERM" "$(echo "$SECONDS_TO_EXIT > 0.1" | bc)" "1"
check "but it exits CLEANLY: a run we stopped would be filed as finished" "$STATUS" "0"

echo
if [ "$fail" = 0 ]; then echo "all assertions held"; else echo "REGRESSION"; fi
echo "raw streams: $WORK"
exit "$fail"
