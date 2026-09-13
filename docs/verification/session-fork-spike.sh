#!/usr/bin/env bash
#
# Can one provider session be forked into two divergent branches?
#
# The fork-graph UI rests on this and nothing in fixtures/ proves it: the
# recorded `resume` scenario is linear (resume-1 -> resume-2). The 2.1.270 flag
# record documents `--fork-session`, `--session-id` and `--resume-session-at`
# from `--help` text only. §16's rule is that help text is not evidence.
#
# Asserts rather than prints: exits non-zero on any regression. Costs a few
# cents and a couple of minutes.
#
#   bash docs/verification/session-fork-spike.sh
#
# Env: CLAUDE_BIN (default: claude), MODEL (default: sonnet)

set -uo pipefail

CLAUDE_BIN="${CLAUDE_BIN:-claude}"
MODEL="${MODEL:-sonnet}"
WORK="$(mktemp -d -t aw-fork-XXXXXX)"
SANDBOX="$WORK/sandbox"

fail=0
ok()   { printf '  ok       %s\n' "$1"; }
bad()  { printf '  FAIL     %s\n' "$1"; fail=1; }
check(){ if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (want '$3', got '$2')"; fi; }
differs(){ if [ "$2" != "$3" ]; then ok "$1"; else bad "$1 (both '$2')"; fi; }
contains(){ case "$2" in *"$3"*) ok "$1";; *) bad "$1 (want '$3' in '$2')";; esac }
lacks(){ case "$2" in *"$3"*) bad "$1 (found '$3' in '$2')";; *) ok "$1";; esac }

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

# ask <label> <prompt> [extra claude args...]
# Writes the raw stream to $WORK/<label>.ndjson. Sets two globals:
#   REPLY_TEXT  the assistant's final text, lowercased
#   SID         the session id the *result* event reports
ask() {
  local label="$1" prompt="$2"; shift 2
  ( cd "$SANDBOX" && timeout 180 "$CLAUDE_BIN" \
      --output-format stream-json --verbose --strict-mcp-config \
      --model "$MODEL" --max-budget-usd 0.20 --max-turns 3 \
      "$@" -p "$prompt" ) > "$WORK/$label.ndjson" 2> "$WORK/$label.stderr"
  REPLY_TEXT="$(jq -r 'select(.type=="result") | .result // ""' "$WORK/$label.ndjson" 2>/dev/null | tr '[:upper:]' '[:lower:]' | tr -d '\n')"
  SID="$(jq -r 'select(.type=="result") | .session_id // empty' "$WORK/$label.ndjson" 2>/dev/null | head -1)"
  INIT_SID="$(jq -r 'select(.type=="system" and .subtype=="init") | .session_id // empty' "$WORK/$label.ndjson" 2>/dev/null | head -1)"
  printf '  [%s] session=%s reply=%s\n' "$label" "${SID:-none}" "${REPLY_TEXT:0:60}"
}

# ---------------------------------------------------------------- the trunk --
echo
echo "trunk: establish a fact under a session id we assign"
S_TRUNK="$(uuid)"
ask trunk "Remember the codeword ZEPHYR. Reply with just: OK" --session-id "$S_TRUNK"
check "trunk uses the session id we assigned" "$SID" "$S_TRUNK"
check "init and result agree on the session id" "$INIT_SID" "$S_TRUNK"

# ------------------------------------------------------------- fork twice ----
echo
echo "fork A and fork B, both resuming the trunk with --fork-session"
ask forkA "Remember the number 7 as well. Reply with just: OK" --resume "$S_TRUNK" --fork-session
S_A="$SID"
ask forkB "Remember the animal OTTER as well. Reply with just: OK" --resume "$S_TRUNK" --fork-session
S_B="$SID"

differs "fork A got a new session id, not the trunk's" "$S_A" "$S_TRUNK"
differs "fork B got a new session id, not the trunk's" "$S_B" "$S_TRUNK"
differs "the two forks got different session ids"      "$S_A" "$S_B"

# ------------------------------------------------- do the branches diverge? --
echo
echo "divergence: each branch should know the trunk and its own turn, not its sibling's"
ask readA "What codeword, number and animal are you holding? Reply with only the values you actually know, comma separated. If you do not know one, omit it." --resume "$S_A"
contains "branch A inherited the trunk's codeword" "$REPLY_TEXT" "zephyr"
contains "branch A knows its own number"           "$REPLY_TEXT" "7"
lacks    "branch A cannot see B's animal"          "$REPLY_TEXT" "otter"

ask readB "What codeword, number and animal are you holding? Reply with only the values you actually know, comma separated. If you do not know one, omit it." --resume "$S_B"
contains "branch B inherited the trunk's codeword" "$REPLY_TEXT" "zephyr"
contains "branch B knows its own animal"           "$REPLY_TEXT" "otter"
lacks    "branch B cannot see A's number"          "$REPLY_TEXT" "7"

# ------------------------------------------------ is the trunk still intact? --
echo
echo "the trunk must survive being forked twice"
ask readTrunk "What codeword, number and animal are you holding? Reply with only the values you actually know, comma separated. If you do not know one, omit it." --resume "$S_TRUNK"
check    "resuming the trunk stays on the trunk id" "$SID" "$S_TRUNK"
contains "the trunk still knows its codeword"       "$REPLY_TEXT" "zephyr"
lacks    "the trunk did not absorb fork A"          "$REPLY_TEXT" "7"
lacks    "the trunk did not absorb fork B"          "$REPLY_TEXT" "otter"

# ------------------------------------- can WE name the fork? (§14 depends on it)
echo
echo "can the fork take a session id we assign, rather than one it invents?"
S_NAMED="$(uuid)"
ask forkNamed "Reply with just: OK" --resume "$S_TRUNK" --fork-session --session-id "$S_NAMED"
if [ -z "$SID" ]; then
  bad "--fork-session with --session-id produced no result event (see $WORK/forkNamed.stderr)"
  head -c 300 "$WORK/forkNamed.stderr"; echo
else
  check "the fork used the id we assigned" "$SID" "$S_NAMED"
fi

# --------------------------------------------- fork from a mid-conversation point
# The graph feature is not "branch from the tip", it is "branch from block N".
# --resume-session-at takes a chain-entry uuid out of the session transcript.
echo
echo "fork from a point in the middle, not just the tip"
PROJ_DIR="$HOME/.claude/projects/$(echo "$SANDBOX" | sed 's|/|-|g')"
TRANSCRIPT="$PROJ_DIR/$S_TRUNK.jsonl"
if [ ! -f "$TRANSCRIPT" ]; then
  echo "  SKIP     no transcript at $TRANSCRIPT — session storage layout differs"
else
  echo "  transcript: $TRANSCRIPT ($(wc -l < "$TRANSCRIPT") entries)"
  FIRST_USER_UUID="$(jq -r 'select(.type=="user") | .uuid // empty' "$TRANSCRIPT" 2>/dev/null | head -1)"
  if [ -z "$FIRST_USER_UUID" ]; then
    echo "  SKIP     no chain-entry uuid found in the transcript"
  else
    ask forkAt "What codeword, number and animal are you holding? Reply with only the values you actually know, comma separated." \
      --resume "$S_TRUNK" --fork-session --resume-session-at "$FIRST_USER_UUID"
    if [ -z "$SID" ]; then
      bad "--resume-session-at was refused (see $WORK/forkAt.stderr)"
      head -c 300 "$WORK/forkAt.stderr"; echo
    else
      differs "truncating fork got its own session id" "$SID" "$S_TRUNK"
      contains "truncating fork still has the trunk prefix" "$REPLY_TEXT" "zephyr"
    fi
  fi
fi

# ------------------------------------------------------------------- verdict --
echo
if [ "$fail" = "0" ]; then
  echo "ALL PASS — a session forks into divergent branches; the graph is buildable."
else
  echo "FAILURES ABOVE — raw streams kept in $WORK"
fi
echo "streams: $WORK"
exit "$fail"
