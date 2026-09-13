#!/usr/bin/env bash
#
# Is the stdout `uuid` the fork address?
#
# `--resume-session-at` takes a chain-entry uuid. The session-fork spike read
# that uuid out of the provider's transcript at
# ~/.claude/projects/<slug>/<session>.jsonl. What we *store* is the `uuid` off
# each stdout stream-json line, as `raw_events.idempotency_key`
# (normalizer/src/claude_code.rs:177). If those are different namespaces then
# forking at a message means reading a storage layout we do not own at fork
# time, and branching-surface.md's fork-anchor decision has to be reopened.
#
# Two questions, and the second is the one the surface actually needs:
#
#   Q1  is a stream uuid the same identifier as a transcript chain-entry uuid,
#       as a set and not as a lucky sample?
#   Q2  is an *assistant* entry accepted as a branch point? The old spike only
#       branched from a user entry; the surface forks from assistant messages
#       far more often.
#
# The trunk is built so truncation is observable: fact A in turn 1, fact B in
# turn 2, fork between them. A fork that silently resumes from the tip fails
# these assertions rather than passing them.
#
# Asserts rather than prints: exits non-zero on any failure. Costs a few cents
# and a couple of minutes.
#
#   bash docs/verification/fork-address-spike.sh
#
# Env: CLAUDE_BIN (default: claude), MODEL (default: sonnet)

set -uo pipefail

CLAUDE_BIN="${CLAUDE_BIN:-claude}"
MODEL="${MODEL:-sonnet}"
WORK="$(mktemp -d -t aw-addr-XXXXXX)"
SANDBOX="$WORK/sandbox"

fail=0
ok()   { printf '  ok       %s\n' "$1"; }
bad()  { printf '  FAIL     %s\n' "$1"; fail=1; }
check(){ if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (want '$3', got '$2')"; fi; }
differs(){ if [ "$2" != "$3" ]; then ok "$1"; else bad "$1 (both '$2')"; fi; }
contains(){ case "$2" in *"$3"*) ok "$1";; *) bad "$1 (want '$3' in '$2')";; esac }
lacks(){ case "$2" in *"$3"*) bad "$1 (found '$3' in '$2')";; *) ok "$1";; esac }

command -v "$CLAUDE_BIN" >/dev/null || { echo "no claude on PATH" >&2; exit 1; }
command -v jq >/dev/null || { echo "no jq on PATH" >&2; exit 1; }
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
# Writes the raw stream to $WORK/<label>.ndjson. Sets:
#   REPLY_TEXT  the assistant's final text, lowercased
#   SID         the session id the *result* event reports
#   INIT_SID    the session id the system/init event reports
#   SUBTYPE     the result event's subtype ("success", "error_during_execution")
#   ERRORS      the result event's errors[], joined
#
# A refused --resume-session-at still emits a result event, so SID alone says
# nothing about acceptance — SUBTYPE is the thing to assert on.
ask() {
  local label="$1" prompt="$2"; shift 2
  ( cd "$SANDBOX" && timeout 180 "$CLAUDE_BIN" \
      --output-format stream-json --verbose --strict-mcp-config \
      --model "$MODEL" --max-budget-usd 0.20 --max-turns 3 \
      "$@" -p "$prompt" ) > "$WORK/$label.ndjson" 2> "$WORK/$label.stderr"
  REPLY_TEXT="$(jq -r 'select(.type=="result") | .result // ""' "$WORK/$label.ndjson" 2>/dev/null | tr '[:upper:]' '[:lower:]' | tr -d '\n')"
  SID="$(jq -r 'select(.type=="result") | .session_id // empty' "$WORK/$label.ndjson" 2>/dev/null | head -1)"
  INIT_SID="$(jq -r 'select(.type=="system" and .subtype=="init") | .session_id // empty' "$WORK/$label.ndjson" 2>/dev/null | head -1)"
  SUBTYPE="$(jq -r 'select(.type=="result") | .subtype // empty' "$WORK/$label.ndjson" 2>/dev/null | head -1)"
  ERRORS="$(jq -r 'select(.type=="result") | (.errors // []) | join("; ")' "$WORK/$label.ndjson" 2>/dev/null | head -1)"
  printf '  [%s] session=%s result=%s reply=%s\n' "$label" "${SID:-none}" "${SUBTYPE:-none}" "${REPLY_TEXT:0:60}"
}

# The question every branch is asked. Deliberately allows "I do not know one".
RECALL="What codeword and animal are you holding? Reply with only the values you actually know, comma separated. If you do not know one, omit it."

# ---------------------------------------------------------------- the trunk --
# Two turns in one session so there is a middle to fork at: A then B.
echo
echo "trunk: fact A in turn one, fact B in turn two, one session id we assign"
S_TRUNK="$(uuid)"
ask t1 "Remember the codeword ZEPHYR. Reply with just: OK" --session-id "$S_TRUNK"
check "turn one uses the session id we assigned" "$SID" "$S_TRUNK"
ask t2 "Also remember the animal OTTER. Reply with just: OK" --resume "$S_TRUNK"
check "a plain resume stays on the trunk id" "$SID" "$S_TRUNK"

ask tTip "$RECALL" --resume "$S_TRUNK"
contains "the trunk tip knows A" "$REPLY_TEXT" "zephyr"
contains "the trunk tip knows B" "$REPLY_TEXT" "otter"

# ------------------------------------------------------- addresses on stdout --
# Which stream lines carry a uuid at all, and which of them are messages. This
# is exactly what the normalizer stores as idempotency_key.
echo
echo "stdout: which lines carry a uuid"
for f in t1 t2 tTip; do
  jq -r 'select(.uuid) | [.type, (.subtype // "-"), .uuid] | @tsv' "$WORK/$f.ndjson"
done > "$WORK/stream-uuids.tsv"
awk -F'\t' '{print $1"/"$2}' "$WORK/stream-uuids.tsv" | sort | uniq -c | sed 's/^/    /'

# Message-bearing stream lines: the ones a feed block comes from.
awk -F'\t' '$1=="assistant" || $1=="user" {print $3}' "$WORK/stream-uuids.tsv" | sort -u > "$WORK/stream-msg.txt"
# Assistant lines only, in stream order — these are the fork points the surface uses.
for f in t1 t2; do
  jq -r 'select(.type=="assistant") | .uuid // empty' "$WORK/$f.ndjson"
done > "$WORK/assistant-order.txt"
A1_UUID="$(sed -n '1p' "$WORK/assistant-order.txt")"
A_LAST_UUID="$(tail -1 "$WORK/assistant-order.txt")"

if [ -z "$A1_UUID" ]; then
  bad "no assistant uuid on stdout at all — nothing in the stream is addressable"
else
  ok "stdout carries an assistant uuid ($A1_UUID)"
fi
differs "turn one and the last turn have different assistant uuids" "$A1_UUID" "$A_LAST_UUID"

# ----------------------------------------------------------- the transcript --
# The slug is the sandbox path with every / replaced by -.
echo
echo "transcript: locate it, with a bounded wait — the provider writes it async"
PROJ_DIR="$HOME/.claude/projects/$(echo "$SANDBOX" | sed 's|/|-|g')"
TRANSCRIPT="$PROJ_DIR/$S_TRUNK.jsonl"
found=""
for _ in $(seq 1 30); do
  if [ -f "$TRANSCRIPT" ] && grep -q "$A_LAST_UUID" "$TRANSCRIPT" 2>/dev/null; then found=1; break; fi
  # The layout may not be <session>.jsonl; take any file in the project dir
  # that carries the trunk's session id.
  if [ -d "$PROJ_DIR" ]; then
    alt="$(grep -l "\"sessionId\":\"$S_TRUNK\"" "$PROJ_DIR"/*.jsonl 2>/dev/null | head -1)"
    if [ -n "$alt" ] && grep -q "$A_LAST_UUID" "$alt" 2>/dev/null; then
      TRANSCRIPT="$alt"; found=1; break
    fi
  fi
  sleep 1
done

if [ -z "$found" ]; then
  # NOT a skip. The fallback plan for a different namespace is "read the
  # provider's transcript at fork time"; if the transcript cannot be found,
  # that fallback does not exist either, and that is the finding.
  bad "no transcript for the trunk under $PROJ_DIR — the read-provider-files fallback is unavailable too"
  ls -la "$PROJ_DIR" 2>&1 | head -20
  echo
  echo "FAILURES ABOVE — raw streams kept in $WORK"
  exit 1
fi
echo "  transcript: $TRANSCRIPT ($(wc -l < "$TRANSCRIPT") entries)"

jq -r 'select(.uuid) | [(.type // "-"), .uuid] | @tsv' "$TRANSCRIPT" > "$WORK/transcript-uuids.tsv"
awk -F'\t' '{print $1}' "$WORK/transcript-uuids.tsv" | sort | uniq -c | sed 's/^/    /'
awk -F'\t' '{print $2}' "$WORK/transcript-uuids.tsv" | sort -u > "$WORK/transcript-all.txt"
awk -F'\t' '$1=="assistant" {print $2}' "$WORK/transcript-uuids.tsv" | sort -u > "$WORK/transcript-asst.txt"

# ------------------------------------------------- Q1: the same namespace? ---
echo
echo "Q1: are the stream uuids and the transcript chain-entry uuids one set?"
MISSING="$(comm -23 "$WORK/stream-msg.txt" "$WORK/transcript-all.txt" | tr '\n' ' ')"
check "every message uuid on stdout is a transcript chain entry" "${MISSING:-none}" "none"

# And the other direction, on the entries the surface forks from.
ASST_ONLY_IN_TRANSCRIPT="$(comm -23 "$WORK/transcript-asst.txt" "$WORK/stream-msg.txt" | tr '\n' ' ')"
check "every transcript assistant entry appeared on stdout" "${ASST_ONLY_IN_TRANSCRIPT:-none}" "none"

printf '    stream message uuids: %s · transcript uuids: %s · transcript assistant uuids: %s\n' \
  "$(wc -l < "$WORK/stream-msg.txt")" "$(wc -l < "$WORK/transcript-all.txt")" "$(wc -l < "$WORK/transcript-asst.txt")"
printf '    transcript entries with no stdout line: %s\n' \
  "$(comm -13 "$WORK/stream-msg.txt" "$WORK/transcript-all.txt" | wc -l)"

# The reverse direction is not symmetric, and the asymmetry is the useful part:
# on a text-only trunk the user's own prompts are chain entries that never reach
# stdout, so they have no idempotency_key and are not addressable from what we
# store. The surface forks from assistant messages, so this costs nothing — but
# it is why "every keyed block is forkable" is the true statement and "every
# chain entry is keyed" is not.
awk -F'\t' '$1=="user" {print $2}' "$WORK/transcript-uuids.tsv" | sort -u > "$WORK/transcript-user.txt"
check "user chain entries of a text-only trunk never reach stdout" \
  "$(comm -12 "$WORK/transcript-user.txt" "$WORK/stream-msg.txt" | wc -l)" "0"

# Set membership is necessary, not sufficient: the flag has to accept one.
# That is Q2, below, and it is deliberately fed from the *stream*.

# --------------------------------------- Q2: fork from an assistant message --
echo
echo "Q2: fork at the turn-one ASSISTANT uuid, taken off the stream"
echo "    it must know A and must NOT know B — a tip resume fails here"
ask forkAsst "$RECALL" --resume "$S_TRUNK" --fork-session --resume-session-at "$A1_UUID"
if [ "$SUBTYPE" != "success" ]; then
  bad "--resume-session-at refused a stream-sourced assistant uuid ($SUBTYPE: $ERRORS)"
else
  ok      "--resume-session-at accepted a stream-sourced assistant uuid"
  differs "the assistant-anchored fork got its own session id" "$SID" "$S_TRUNK"
  contains "the branch kept the prefix: it knows A" "$REPLY_TEXT" "zephyr"
  lacks    "the branch truncated: it does NOT know B" "$REPLY_TEXT" "otter"
fi

# Positive control. If the flag ignored our uuid and always rewound to the
# start, the assertion above would pass for the wrong reason. Anchoring at the
# LAST assistant uuid must keep both facts.
echo
echo "control: fork at the LAST assistant uuid — the address must select a point,"
echo "         not always rewind, so this branch must know both A and B"
ask forkTip "$RECALL" --resume "$S_TRUNK" --fork-session --resume-session-at "$A_LAST_UUID"
if [ "$SUBTYPE" != "success" ]; then
  bad "--resume-session-at refused the last assistant uuid ($SUBTYPE: $ERRORS)"
else
  differs "the late fork got its own session id" "$SID" "$S_TRUNK"
  contains "the late branch knows A" "$REPLY_TEXT" "zephyr"
  contains "the late branch knows B" "$REPLY_TEXT" "otter"
fi

# --------------------------------------------- the fan-out suffix is not an address
# One provider line can fan out into several events, keyed {uuid}#{i}. The
# record claims the address is the part before the '#'. Confirm the suffixed
# form is not what the flag takes — and, more importantly, that an address the
# provider cannot resolve is REFUSED rather than quietly rounded to the tip.
echo
echo "fan-out: {uuid}#{i} is our key, not the provider's address"
ask forkSuffix "$RECALL" --resume "$S_TRUNK" --fork-session --resume-session-at "$A1_UUID#0"
check    "the suffixed form is refused, so the address is the part before the '#'" "$SUBTYPE" "error_during_execution"
contains "and it is refused by name, not silently" "$ERRORS" "No message found with message.uuid of: $A1_UUID#0"
lacks    "a refused fork produced no answer, so it did not resume the tip" "$REPLY_TEXT" "otter"

echo
echo "no silent fallback: an address that resolves nowhere must be an error"
ask forkBogus "$RECALL" --resume "$S_TRUNK" --fork-session --resume-session-at "$(uuid)"
check "an unknown uuid is refused rather than rounded to the nearest entry" "$SUBTYPE" "error_during_execution"
lacks "the refused run answered nothing" "$REPLY_TEXT" "zephyr"

# -------------------------------------------- the trunk survives all of this --
echo
echo "the trunk must be unchanged by three forks"
ask readTrunk "$RECALL" --resume "$S_TRUNK"
check    "resuming the trunk stays on the trunk id" "$SID" "$S_TRUNK"
contains "the trunk still knows A" "$REPLY_TEXT" "zephyr"
contains "the trunk still knows B" "$REPLY_TEXT" "otter"

# ------------------------------------------------------------------- verdict --
echo
if [ "$fail" = "0" ]; then
  echo "ALL PASS — the stdout uuid IS the fork address, assistant entries included."
else
  echo "FAILURES ABOVE — raw streams kept in $WORK"
fi
echo "streams: $WORK"
exit "$fail"
