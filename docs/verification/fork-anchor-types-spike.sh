#!/usr/bin/env bash
#
# WHICH keyed blocks are forkable? The address spike said "all of them", and the
# built surface found a counter-example on its first day.
#
# `docs/verification/claude-code-fork-address.md` proved the stdout `uuid` is the
# identifier `--resume-session-at` accepts, and `branching-surface.md` turned
# that into a rule: "a block whose `idempotency_key` is a bare uuid has an
# address." The normalizer keys *every* uuid-bearing stdout line
# (`crates/normalizer/src/claude_code.rs`), so that rule offers a fork toolbar on
# every one of them.
#
# Driving the real UI then produced a refusal: a fork anchored at a `system` /
# `hook_started` line — keyed, therefore offered — came back
# `error_during_execution`, no turns, $0.00. The address spike could not have
# seen it: its trunk was text-only and it read uuids off `assistant` and `user`
# lines only.
#
# One observation is not a rule. This buys the rule:
#
#   Q1  for each `type`/`subtype` combination that carries a `uuid`, is that uuid
#       accepted by `--resume-session-at`, or refused?
#   Q2  is the accepted set predictable from `type` alone, or does it need
#       `subtype` too?
#   Q3  does a TOOL-BEARING conversation fork? The address spike left this
#       explicitly unproven — its trunk was text-only on purpose — and it is the
#       common case in real work. So this trunk runs a tool in every turn, and
#       the `assistant` line carrying a `tool_use` block and the `user` line
#       carrying the matching `tool_result` are both anchored at.
#   Q4  and the `user` line the *product* produces: the driver always passes
#       `--replay-user-messages`, which echoes the user's own prompt back onto
#       stdout with a uuid of its own. That line is keyed and offered too, and
#       no `-p` run can produce it.
#
# The trunk is built so truncation is observable, as the address spike's was:
# fact A enters the chain in turn one, fact B in turn two, and a fork anchored in
# turn one must know A and must NOT know B. A fork that silently resumed the tip
# would know B and fail these assertions rather than pass them.
#
# Both facts arrive in the *prompt* of their turn rather than out of a tool
# result, deliberately: a tool result lands in the middle of the turn's stream,
# so an anchor earlier in the same turn would truncate the fact away and "knows
# A" would be a statement about where in the turn the anchor sat rather than
# about the fork. Facts in prompts make every turn-one anchor equivalent.
#
# Acceptance is read off the result's `subtype` and NEVER off having received a
# session id: the address spike proved a refused fork still gets a fresh one, and
# its own first draft made that mistake.
#
# Asserts rather than prints: exits non-zero on any failure. Sequential, in a
# throwaway git sandbox. Costs a few tens of cents and about five minutes.
#
#   bash docs/verification/fork-anchor-types-spike.sh
#
# Env: CLAUDE_BIN (default: claude), MODEL (default: sonnet)

set -uo pipefail

CLAUDE_BIN="${CLAUDE_BIN:-claude}"
MODEL="${MODEL:-sonnet}"
WORK="$(mktemp -d -t aw-anchor-XXXXXX)"
SANDBOX="$WORK/sandbox"

fail=0
ok()   { printf '  ok       %s\n' "$1"; }
bad()  { printf '  FAIL     %s\n' "$1"; fail=1; }
note() { printf '           %s\n' "$1"; }
check(){ if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (want '$3', got '$2')"; fi; }
differs(){ if [ "$2" != "$3" ]; then ok "$1"; else bad "$1 (both '$2')"; fi; }
contains(){ case "$2" in *"$3"*) ok "$1";; *) bad "$1 (want '$3' in '$2')";; esac }
lacks(){ case "$2" in *"$3"*) bad "$1 (found '$3' in '$2')";; *) ok "$1";; esac }

command -v "$CLAUDE_BIN" >/dev/null || { echo "no claude on PATH" >&2; exit 1; }
command -v jq >/dev/null || { echo "no jq on PATH" >&2; exit 1; }
echo "claude: $($CLAUDE_BIN --version)"
echo "work:   $WORK"

mkdir -p "$SANDBOX"
git -C "$SANDBOX" init -q -b main >/dev/null 2>&1
git -C "$SANDBOX" config user.email "fixtures@example.com"
git -C "$SANDBOX" config user.name "Fixture Recorder"
echo "placeholder" > "$SANDBOX/README.md"
# Something for a tool to touch, so every turn is tool-bearing (Q3). Its
# contents are deliberately inert: the facts live in the prompts.
echo "this file exists so that a turn runs a tool" > "$SANDBOX/fact.txt"
git -C "$SANDBOX" add -A
git -C "$SANDBOX" commit -qm "init" >/dev/null 2>&1

uuid() { python3 -c 'import uuid;print(uuid.uuid4())'; }

COST_FILE="$WORK/costs"
: > "$COST_FILE"

# ask <label> <prompt> [extra claude args...]
# Writes the raw stream to $WORK/<label>.ndjson. Sets:
#   REPLY_TEXT  the final result text, lowercased
#   SID         the session id the *result* event reports
#   SUBTYPE     the result event's subtype ("success", "error_during_execution")
#   ERRORS      the result event's errors[], joined
ask() {
  local label="$1" prompt="$2"; shift 2
  ( cd "$SANDBOX" && timeout 240 "$CLAUDE_BIN" \
      --output-format stream-json --verbose --strict-mcp-config \
      --model "$MODEL" --max-budget-usd 0.20 \
      "$@" -p "$prompt" ) > "$WORK/$label.ndjson" 2> "$WORK/$label.stderr"
  read_result "$label"
}

# read_result <label> — pull the scoring fields out of a stream, whether or not
# this script spawned the process itself. Shared by `ask` and `ask_stream` so
# one turn cannot be scored by a different rule than another.
read_result() {
  local label="$1"
  REPLY_TEXT="$(jq -r 'select(.type=="result") | .result // ""' "$WORK/$label.ndjson" 2>/dev/null | tr '[:upper:]' '[:lower:]' | tr -d '\n')"
  SID="$(jq -r 'select(.type=="result") | .session_id // empty' "$WORK/$label.ndjson" 2>/dev/null | head -1)"
  SUBTYPE="$(jq -r 'select(.type=="result") | .subtype // empty' "$WORK/$label.ndjson" 2>/dev/null | head -1)"
  ERRORS="$(jq -r 'select(.type=="result") | (.errors // []) | join("; ")' "$WORK/$label.ndjson" 2>/dev/null | head -1)"
  jq -r 'select(.type=="result") | .total_cost_usd // 0' "$WORK/$label.ndjson" 2>/dev/null >> "$COST_FILE"
  printf '  [%s] session=%s result=%s reply=%s\n' \
    "$label" "${SID:0:8}" "${SUBTYPE:-none}" "${REPLY_TEXT:0:60}"
}

# ask_stream <label> <prompt> [extra claude args...]
#
# One turn driven the way the *driver* drives one: `--input-format stream-json`
# with `--replay-user-messages`, both unconditional in
# `crates/driver/src/launch.rs`. It matters here and nowhere else: under those
# flags the user's own prompt comes back on stdout as a `user` line carrying a
# uuid, so it is keyed, stored as `user.message`, and offered a fork toolbar.
# The `-p` turns above cannot produce that line, so without this the rule would
# be written from a configuration the product does not use.
ask_stream() {
  local label="$1" prompt="$2"; shift 2
  python3 - "$CLAUDE_BIN" "$SANDBOX" "$WORK/$label.ndjson" "$MODEL" "$prompt" "$@" \
    << 'PYDRIVER' 2> "$WORK/$label.stderr"
import json, subprocess, sys, threading

claude, sandbox, out, model, prompt, *extra = sys.argv[1:]
argv = [claude, "--output-format", "stream-json", "--verbose",
        "--strict-mcp-config", "--model", model, "--max-budget-usd", "0.20",
        "--input-format", "stream-json", "--replay-user-messages", *extra, "-p"]
p = subprocess.Popen(argv, cwd=sandbox, stdin=subprocess.PIPE,
                     stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
threading.Timer(240, p.kill).start()
p.stdin.write(json.dumps({
    "type": "user",
    "message": {"role": "user", "content": [{"type": "text", "text": prompt}]},
    "parent_tool_use_id": None,
}) + "\n")
p.stdin.flush()
with open(out, "w") as fh:
    for line in p.stdout:
        fh.write(line)
        fh.flush()
        try:
            msg = json.loads(line)
        except Exception:
            continue
        if msg.get("type") == "result":
            break
try:
    p.stdin.close()
except Exception:
    pass
p.terminate()
PYDRIVER
  read_result "$label"
}

# The question every branch is asked. Deliberately allows "I do not know one".
RECALL="What codeword and animal are you holding? Reply with only the values you actually know, comma separated. If you do not know one, omit it. Do not use any tools."

# ---------------------------------------------------------------- the trunk --
# Five turns in one session. Turn one is where every truncation-testable anchor
# comes from; turns two to five exist to make the *rest* of the provider's
# vocabulary appear on a stream that belongs to this chain — including the one
# line only the driver's own flags produce (see `ask_stream`).
echo
echo "trunk: fact A in turn one, fact B in turn two, every turn runs a tool"
S_TRUNK="$(uuid)"

ask t1 "Remember the codeword ZEPHYR. Read fact.txt, then reply with just: OK" \
  --session-id "$S_TRUNK" --max-turns 4 --allowed-tools "Read"
check "turn one uses the session id we assigned" "$SID" "$S_TRUNK"
check "turn one succeeded" "$SUBTYPE" "success"

ask t2 "Also remember the animal OTTER. Read fact.txt again, then reply with the codeword and the animal you are holding, comma separated." \
  --resume "$S_TRUNK" --max-turns 4 --allowed-tools "Read"
check    "a plain resume stays on the trunk id" "$SID" "$S_TRUNK"
contains "the trunk knows A" "$REPLY_TEXT" "zephyr"
contains "the trunk knows B" "$REPLY_TEXT" "otter"

# `result/error_max_turns`: a turn that cannot finish inside its budget. The
# only way to get a non-`success` result subtype onto a chain we own.
ask t3 "Read fact.txt and summarise it in one sentence." \
  --resume "$S_TRUNK" --max-turns 1 --allowed-tools "Read"

# `system/permission_denied`: a tool nobody is able to approve. And
# `--include-partial-messages` so `stream_event` appears somewhere in the chain.
ask t4 "Create a file notes.txt in the repo root containing the single word hello." \
  --resume "$S_TRUNK" --max-turns 3 --permission-prompts none --include-partial-messages

# `user/-/text`: the user's own prompt, echoed back. Only reachable under the
# driver's own flags, which is exactly why it is here — see `ask_stream`.
ask_stream t5 "Say READY and nothing else." --resume "$S_TRUNK" --max-turns 3

# -------------------------------------------------------- the anchor table ---
# One anchor per distinct kind. A kind is `type/subtype/content`, because the
# open question is whether `type` is enough — and because an `assistant` line
# carrying a `tool_use` block and one carrying `text` are the same `type` but
# very different moments in a conversation.
echo
echo "anchors: one uuid per type/subtype/content-block kind, in stream order"
kinds_of() { # <ndjson>
  jq -r '
    select(.uuid) |
    ( if (.type=="assistant" or .type=="user")
      then ( [.message.content[]?.type] | unique | join("+") )
      else "" end ) as $k |
    [ (.type + "/" + (.subtype // "-") + "/" + (if ($k|length)==0 then "-" else $k end)), .uuid ]
    | @tsv' "$1"
}

# Turn one, split at its first `assistant` line.
#
#   earlyA  at or after the first assistant line. Turn one's prompt — and so
#           fact A — is already in the chain, so an accepted fork must know A
#           and must not know B.
#   early   before it: hook lifecycle and `init` fire ahead of the prompt
#           entering the chain, so whether A survives is a statement about the
#           provider's ordering rather than about the fork. Only "not B" is
#           asserted for these, and that is the assertion that matters — it is
#           the one a silent tip-resume fails.
#   late    turns two to four. Both facts are behind the anchor, so an accepted
#           fork must know both. These are the positive control: if every fork
#           came back knowing only A, the address would be rewinding rather than
#           selecting.
A1_LINE="$(jq -r 'select(.type=="assistant") | input_line_number' "$WORK/t1.ndjson" 2>/dev/null | head -1)"
[ -n "$A1_LINE" ] || A1_LINE=1
kinds_of "$WORK/t1.ndjson" > "$WORK/t1.kinds"
: > "$WORK/anchors.tsv"
n=0
while IFS=$'\t' read -r kind u; do
  n=$((n + 1))
  # `kinds_of` preserves stream order, so the nth kind row is at or after the
  # nth uuid-bearing line; comparing against the first assistant line's number
  # is a conservative split.
  if [ "$n" -ge "$A1_LINE" ]; then phase=earlyA; else phase=early; fi
  printf '%s\t%s\t%s\n' "$kind" "$u" "$phase" >> "$WORK/anchors.tsv"
done < "$WORK/t1.kinds"
for f in t2 t3 t4 t5; do
  kinds_of "$WORK/$f.ndjson" | while IFS=$'\t' read -r kind u; do
    printf '%s\t%s\tlate\n' "$kind" "$u"
  done >> "$WORK/anchors.tsv"
done
# First occurrence of each kind wins, which keeps turn-one anchors (where
# truncation is observable) ahead of later ones.
awk -F'\t' '!seen[$1]++' "$WORK/anchors.tsv" > "$WORK/anchors.uniq"
mv "$WORK/anchors.uniq" "$WORK/anchors.tsv"
awk -F'\t' '{printf "    %-34s %s  %s\n", $1, substr($2,1,8), $3}' "$WORK/anchors.tsv"

ANCHOR_COUNT="$(wc -l < "$WORK/anchors.tsv")"
if [ "$ANCHOR_COUNT" -lt 6 ]; then
  bad "only $ANCHOR_COUNT anchor kinds on the trunk — the vocabulary did not appear, so this run proves little"
fi
# Q3's precondition. If no tool ran, the tool-bearing half of this spike is not
# being tested and must say so rather than pass quietly.
if grep -q 'assistant/-/tool_use' "$WORK/anchors.tsv"; then
  ok "the trunk is tool-bearing: an assistant line carries a tool_use block"
else
  bad "no tool_use block on the trunk — Q3 is untested, not passed"
fi
if grep -q 'user/-/tool_result' "$WORK/anchors.tsv"; then
  ok "the trunk carries a user tool_result line, the other half of a tool call"
else
  bad "no tool_result line on the trunk — Q3 is untested, not passed"
fi
# The product's own shape. Without this line the `user` half of the rule would
# rest on tool results alone.
if grep -q 'user/-/text' "$WORK/anchors.tsv"; then
  ok "the trunk carries a replayed user prompt line, as every driver-launched run does"
else
  bad "no replayed user prompt on the trunk — the product's own user line is untested"
fi

# -------------------------------------------------------------- the matrix ---
# One fork per anchor kind. `--allowed-tools Read` so that a fork anchored
# mid-tool-call can finish the dangling call rather than dying of the harness.
echo
echo "forks: one per anchor kind — accepted or refused, read off result.subtype"
: > "$WORK/matrix.tsv"
while IFS=$'\t' read -r kind u phase; do
  label="fork_$(printf '%s' "$kind" | tr '/+' '__')"
  echo
  echo "  anchor $kind ($phase)"
  ask "$label" "$RECALL" --resume "$S_TRUNK" --fork-session \
    --resume-session-at "$u" --max-turns 3 --allowed-tools "Read"

  knowsA=no; knowsB=no
  case "$REPLY_TEXT" in *zephyr*) knowsA=yes;; esac
  case "$REPLY_TEXT" in *otter*) knowsB=yes;; esac

  case "$SUBTYPE" in
    success)
      printf '%s\taccepted\t%s\t%s\t%s\n' "$kind" "$phase" "$knowsA" "$knowsB" >> "$WORK/matrix.tsv"
      differs "  accepted, and the branch got its own session id" "$SID" "$S_TRUNK"
      case "$phase" in
        earlyA)
          contains "  it kept the prefix: knows A" "$REPLY_TEXT" "zephyr"
          lacks    "  it truncated: does NOT know B" "$REPLY_TEXT" "otter"
          ;;
        early)
          lacks    "  it truncated: does NOT know B" "$REPLY_TEXT" "otter"
          note     "  (A not asserted: this anchor precedes turn one's prompt)"
          ;;
        late)
          contains "  a late anchor keeps A" "$REPLY_TEXT" "zephyr"
          contains "  a late anchor keeps B — the address selects, it does not rewind" "$REPLY_TEXT" "otter"
          ;;
      esac
      ;;
    error_during_execution)
      printf '%s\trefused\t%s\t%s\t%s\n' "$kind" "$phase" "$knowsA" "$knowsB" >> "$WORK/matrix.tsv"
      # A refusal is a finding, not a failure — but it has to be a LOUD one.
      contains "  refused, and refused by name" "$ERRORS" "No message found with message.uuid of: $u"
      check    "  a refused fork answers nothing, so it did not resume the tip" "${REPLY_TEXT:-<empty>}" "<empty>"
      ;;
    *)
      printf '%s\t%s\t%s\t%s\t%s\n' "$kind" "${SUBTYPE:-none}" "$phase" "$knowsA" "$knowsB" >> "$WORK/matrix.tsv"
      bad "  neither accepted nor refused: result subtype '${SUBTYPE:-none}' ($ERRORS)"
      ;;
  esac
done < "$WORK/anchors.tsv"

# --------------------------------------------------------------- the rule ----
echo
echo "the matrix"
printf '    %-34s %-9s %-7s %-6s %s\n' kind outcome phase knowsA knowsB
awk -F'\t' '{printf "    %-34s %-9s %-7s %-6s %s\n", $1, $2, $3, $4, $5}' "$WORK/matrix.tsv"

ACCEPTED_TYPES="$(awk -F'\t' '$2=="accepted"{split($1,p,"/"); print p[1]}' "$WORK/matrix.tsv" | sort -u | tr '\n' ' ')"
REFUSED_TYPES="$(awk -F'\t' '$2=="refused"{split($1,p,"/"); print p[1]}' "$WORK/matrix.tsv" | sort -u | tr '\n' ' ')"

echo
echo "Q1/Q2: is the accepted set 'assistant and user', and nothing else?"
note "accepted types: ${ACCEPTED_TYPES:-none}"
note "refused  types: ${REFUSED_TYPES:-none}"

# The baseline the built surface already depends on. If either of these fails,
# the fork gesture is broken rather than over-offered.
for k in assistant user; do
  if grep -q "^$k/" "$WORK/matrix.tsv"; then
    n_ref="$(awk -F'\t' -v k="$k" '$2=="refused" && index($1, k "/")==1' "$WORK/matrix.tsv" | wc -l)"
    check "every '$k' anchor was accepted" "$n_ref" "0"
  else
    bad "no '$k' anchor on the trunk at all"
  fi
done

# Step 5's counter-example, re-observed. If NOTHING was refused, the finding
# this spike exists to generalise has evaporated and the record is wrong about
# something — say so rather than quietly encoding a filter nobody needs.
n_refused="$(awk -F'\t' '$2=="refused"' "$WORK/matrix.tsv" | wc -l)"
if [ "$n_refused" -gt 0 ]; then
  ok "a keyed block was refused — 'every keyed block is forkable' has a counter-example"
else
  bad "nothing was refused: this contradicts the built surface's observation, and no filter is justified"
fi

# Q2, asserted rather than eyeballed: acceptance must be a function of `type`
# alone. If one subtype of a type is accepted and another refused, the rule the
# crate encodes has to widen to a pair and this assertion is how we find out.
SPLIT="$(awk -F'\t' '{split($1,p,"/"); o[p[1]]=o[p[1]] " " $2} END {for (t in o) {a=index(o[t],"accepted"); r=index(o[t],"refused"); if (a>0 && r>0) printf "%s ", t}}' "$WORK/matrix.tsv")"
check "acceptance is a function of the provider type alone, not of its subtype" "${SPLIT:-none}" "none"

# And the pin. Set from the run recorded in
# `docs/verification/claude-code-fork-anchor-types.md`; a future provider that
# widens or narrows the set trips this rather than passing quietly.
check "the accepted set is exactly {assistant, user}" "$ACCEPTED_TYPES" "assistant user "

# ---------------------------------------------- the trunk survives all of it --
echo
echo "the trunk must be unchanged by every fork above"
ask readTrunk "$RECALL" --resume "$S_TRUNK" --max-turns 2
check    "resuming the trunk stays on the trunk id" "$SID" "$S_TRUNK"
contains "the trunk still knows A" "$REPLY_TEXT" "zephyr"
contains "the trunk still knows B" "$REPLY_TEXT" "otter"

# ------------------------------------------------------------------ verdict --
TOTAL="$(awk '{s+=$1} END {printf "%.4f", s}' "$COST_FILE")"
echo
echo "total cost: \$$TOTAL over $(wc -l < "$COST_FILE") runs"
if [ "$fail" = "0" ]; then
  echo 'ALL PASS - only assistant and user lines are addressable; the rest are keyed and refused.'
else
  echo "FAILURES ABOVE — raw streams kept in $WORK"
fi
echo "streams: $WORK"
exit "$fail"
