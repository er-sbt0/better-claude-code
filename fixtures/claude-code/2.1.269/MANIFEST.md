<!--
Golden fixtures. Discharges the §16 "record fixtures before writing the
normalizer" requirement for Claude Code 2.1.269.

Recorded: 2026-09-12 · Claude Code 2.1.269 · Ubuntu, x86-64 · model alias `sonnet`
          (resolves to claude-sonnet-5; a claude-haiku-4-5 side-call also shows
          up in every run's `modelUsage`).
Regenerate with ./record.sh — see "Reproducing" below.
-->

# Golden `stream-json` fixtures — Claude Code 2.1.269

Every file is the **raw stdout of one real `claude` process**, one JSON object
per line, byte-identical to what was emitted apart from the path/credential
substitutions listed under [Redaction](#redaction). Nothing here is
hand-written. Session uuids, message ids, timestamps, costs and token counts are
left intact on purpose — they are what the ordering, dedupe and metrics tests
need.

## Recording conditions

All runs share:

```
--output-format stream-json --verbose
--strict-mcp-config            # no --mcp-config => zero MCP servers in init
--model sonnet
--max-budget-usd 0.50
```

and every run's cwd is a throwaway git repo (`/FIXTURE_SANDBOX`) containing
`README.md`, `package.json`, `src/calc.ts` (whose `subtract()` wrongly returns
`a + b`) and `src/stats.ts`, with one initial commit. The design repo was never
the cwd of any recorded run.

`-p` (print mode) is implied by `--output-format stream-json`; it is passed
explicitly only where an argument follows it.

**Caveat — ambient configuration is NOT suppressed.** `--strict-mcp-config` only
removes MCP servers. The recording machine's user settings still applied, and
they are visible in the streams: a `SessionStart` hook (the two leading
`system/hook_started` + `system/hook_response` lines in every file), two plugins,
and the `skills` / `slash_commands` / `agents` lists in `init`. Those are
machine-specific and will differ on a re-record. Adding `--setting-sources ""`
or `--bare` would suppress them; that was deliberately *not* done, because a
driver in the field will see exactly this kind of noise and the normalizer has
to tolerate it.

---

## Fixtures

### `text-only.ndjson` — 6787 B, 7 lines

```
claude -p "what is 2+2, answer in one word" --max-turns 3
```

The minimal envelope. Covers: `run.started` / `assistant.message` /
`run.finished`.

```
system/hook_started → system/hook_response → system/init → rate_limit_event
→ assistant(text) → rate_limit_event → result/success
```

Surprising: the minimal run is **not** `init → assistant → result`. Four of the
seven lines are message types §5's table has no row for. `rate_limit_event`
appears both before and after the assistant message — its position is not
stable across runs (compare `resume-2.ndjson`, where it lands *after* the
assistant message, and `error.ndjson`, where there is only one).

### `tool-read.ndjson` — 9476 B, 9 lines

```
claude -p "Read src/stats.ts and describe in one sentence what it does. Do not edit anything." \
  --max-turns 4 --allowed-tools "Read"
```

`Read` tool_use / tool_result round trip.

```
system/hook_started → system/hook_response → system/init → rate_limit_event
→ assistant(thinking) → assistant(tool_use:Read) → user(tool_result)
→ assistant(text) → result/success
```

Surprising: a `thinking` block appears with **no thinking flags passed at all**,
and its `thinking` string is `""` (only `signature` is populated). See
`thinking.ndjson`. Also, the thinking block and the tool_use block are two
separate `assistant` lines that share one `message.id` — see
[Assistant fan-out](#assistant-fan-out).

### `tool-edit.ndjson` — 13017 B, 12 lines — **most load-bearing**

```
claude -p "src/calc.ts has one obvious bug. Fix it with a single Edit, then stop. Do not run any commands." \
  --max-turns 6 --permission-mode acceptEdits --allowed-tools "Read" "Edit"
```

§6 "source 1" — `Edit` with typed args giving changed files exactly. Confirmed:

```json
{"type":"tool_use","id":"toolu_01P34…","name":"Edit",
 "input":{"replace_all":false,
          "file_path":"/FIXTURE_SANDBOX/src/calc.ts",
          "old_string":"export function subtract…",
          "new_string":"export function subtract…"},
 "caller":{"type":"direct"}}
```

`file_path` is **absolute**, not workspace-relative.

```
system/hook_started → system/hook_response → system/init → rate_limit_event
→ assistant(thinking) → assistant(tool_use:Read) → user(tool_result)
→ assistant(text) → assistant(tool_use:Edit) → user(tool_result)
→ assistant(text) → result/success
```

Surprising: the assistant envelope carries a **second, redundant copy** of the
tool inputs at top level under `wire_tool_inputs`, keyed by tool_use id. The
`Edit` tool_result is a prose confirmation string; the structured
before/after lives in `tool_use_result`, not in the `tool_result` block.

### `tool-bash.ndjson` — 7940 B, 8 lines

```
claude -p "Run the command 'git status --short --branch' and tell me its output. Use only that one command." \
  --max-turns 4 --allowed-tools "Bash(git status:*)"
```

Successful Bash. `tool_use_result` =
`{"stdout":"## main","stderr":"","interrupted":false,"isImage":false,"noOutputExpected":false}`.

```
system/hook_started → system/hook_response → system/init → rate_limit_event
→ assistant(tool_use:Bash) → user(tool_result) → assistant(text) → result/success
```

Surprising: **there is no exit-code field on a successful Bash result.** See
`tool-bash-fail.ndjson`.

### `tool-bash-fail.ndjson` — 11849 B, 12 lines — *added, not in the original target set*

```
claude -p "Run the command 'git cat-file -e deadbeef…deadbeef' and report its exit status. Use only that one command." \
  --max-turns 4 --allowed-tools "Bash(git cat-file:*)"
```

Added specifically to test §6's claim that exit codes come for free. They do
not. On failure:

* `tool_result` block: `{"content":"Exit code 1","is_error":true,…}`
* `tool_use_result`: the bare **string** `"Error: Exit code 1"` — not the object
  shape a successful Bash returns.

So the exit code exists only inside English prose. A driver that wants it must
regex `^Exit code (\d+)$`, and must handle `tool_use_result` being either an
object or a string.

```
… → assistant(tool_use:Bash) → system/permission_denied → user(tool_result)
  → assistant(thinking) → assistant(tool_use:Bash) → user(tool_result)
  → assistant(text) → result/success
```

Surprising bonus: the model's first attempt appended `; echo "Exit status: $?"`,
which fell outside the `Bash(git cat-file:*)` rule. That produced a
`system/permission_denied` with `decision_reason_type:"subcommandResults"` and a
`tool_result_meta` of `non_execution_kind:"user-rejected"` — a *different*
denial flavour from `permission-denied.ndjson`, in the same schema.

### `thinking.ndjson` — 18175 B, 52 lines

```
claude -p "Without using any tools, work this out and reply with just the number: what is the
           smallest positive integer n such that n! ends in exactly 100 trailing zeros?" \
  --max-turns 3 --thinking enabled --effort high --thinking-display summarized
```

```
system/hook_started → system/hook_response → system/init → rate_limit_event
→ system/thinking_tokens ×45 → assistant(thinking) → assistant(text) → result/success
```

Two findings, both material:

1. **`--thinking-display summarized` is mandatory to get any thinking text.**
   Without it, print mode still emits `thinking` blocks (see `tool-read`,
   `tool-edit`, `tool-bash-fail`) but `block.thinking` is `""` and only
   `signature` is filled. The allowed values are `summarized` and `omitted`
   only — the raw chain of thought is never on the wire, only a summary.
2. **45 `system/thinking_tokens` progress messages**, one per burst, carrying
   `estimated_tokens` and `estimated_tokens_delta`. These are the bulk of the
   file and are a UI-progress channel with no row in §5's table.

### `thinking-partial.ndjson` — 25017 B, 72 lines — *added, not in the original target set*

Same prompt and flags as `thinking.ndjson`, plus `--include-partial-messages`.
The only capture of `thinking_delta` and `signature_delta`:

```
stream_event/content_block_start(thinking) → thinking_delta ×27 → signature_delta
→ assistant(thinking) → content_block_stop → content_block_start(text) → text_delta
→ assistant(text) → content_block_stop → message_delta → message_stop → result/success
```

Surprising: `thinking_delta` carries an `estimated_tokens` field that is always
`null` here, while the interleaved `system/thinking_tokens` messages carry the
real numbers — two parallel channels for the same quantity.

### `partial-messages.ndjson` — 8761 B, 13 lines

```
claude -p "what is 2+2, answer in one word" --max-turns 3 --include-partial-messages
```

```
system/hook_started → system/hook_response → system/init → system/status
→ rate_limit_event → stream_event/message_start
→ stream_event/content_block_start → stream_event/content_block_delta:text_delta
→ assistant(text)
→ stream_event/content_block_stop → stream_event/message_delta
→ stream_event/message_stop → result/success
```

Surprising, and the single biggest normalizer hazard in this set: the complete
`assistant` message is **still emitted, interleaved between
`content_block_delta` and `content_block_stop`**. The same text therefore
arrives twice. `stream_event.event.message.id` equals the aggregated
`assistant.message.id`, so that id is the dedupe key — but note the aggregate
lands *before* `content_block_stop`, so "aggregate arrives after the block
closes" is a false assumption.

`--include-partial-messages` also adds a `system/status` message
(`{"status":"requesting"}`) that the non-partial runs do not have.

### `multi-turn.ndjson` — 12295 B, 11 lines — **proves `-p` is a live session**

Two user turns written to the stdin of **one** process (`--input-format
stream-json`), the second sent only after the first `result` was read:

```
claude --output-format stream-json --verbose --strict-mcp-config --model sonnet \
  --max-budget-usd 0.50 --input-format stream-json --replay-user-messages \
  --max-turns 6 -p
# stdin, line 1: {"type":"user","message":{"role":"user","content":[{"type":"text","text":"Say the word HELLO and nothing else."}]},"parent_tool_use_id":null}
# stdin, line 2 (after result #1): …"Now say it backwards, and nothing else."
```

```
system/hook_started → system/hook_response → system/init → rate_limit_event
→ user(text,replay) → assistant(text) → result/success
→ system/init → user(text,replay) → assistant(text) → result/success
```

Two surprises, both of which break the §5 mapping:

1. **`system/init` is emitted again at the start of the second turn**, with the
   *same* `session_id` and a *new* `uuid`. `init` is therefore per-turn, not
   per-run: mapping it 1:1 to `run.started` produces a spurious second run.
2. **`result` is emitted per turn, not per run** — twice in one process, with
   `result_index` 0 and 1 and `num_turns: 1` each. `result_index` is the field
   that distinguishes them. `run.finished` must not fire on the first `result`
   when the process is still alive.

`--replay-user-messages` echoes each stdin turn back as a `user` message with
`isReplay: true` and a server-assigned `uuid` — usable as the ack the
verification report describes.

### `resume-1.ndjson` / `resume-2.ndjson` — 6348 B + 6042 B, 6 lines each

```
# run 1
claude -p "Remember the number 42. Reply with just: OK" --max-turns 3 \
  --session-id 633e961c-a701-4d95-b1ca-9e28bd717b14
# run 2, separate process, after run 1 exited
claude -p "What number did I ask you to remember? Reply with just the number." --max-turns 3 \
  --resume 633e961c-a701-4d95-b1ca-9e28bd717b14
```

Both files: `system/hook_started → system/hook_response → system/init →
(rate_limit_event) → assistant(text) → result/success`. Run 2's result is `"42"`,
so the transcript really carried over.

Surprising, and good news for §14: **a resumed stream's opening events are
indistinguishable from a fresh run's.** `resume-2`'s `init` has the same key set
as `resume-1`'s, carries the *same* `session_id` (the one we assigned), and
contains **no marker that this is a resumption** — no replayed history, no
"resumed" flag, no message count. The driver must know it resumed; the stream
will not tell it. `num_turns` restarts at 1. Also note the `--session-id` we
generated was honoured verbatim, confirming the verification report's point that
scraping the session id out of `init` is no longer necessary.

### `permission-denied.ndjson` — 10701 B, 9 lines

```
claude -p "Create a file notes.txt in the repo root containing the single word hello." \
  --max-turns 4 --permission-prompts none
```

```
… → assistant(tool_use:Write) → system/permission_denied → user(tool_result)
  → assistant(text) → result/success
```

The denial path with **no MCP server involved at all**. `system/permission_denied`:

```json
{"type":"system","subtype":"permission_denied","tool_name":"Write",
 "tool_use_id":"toolu_01BdB2…","decision_reason_type":"asyncAgent",
 "decision_reason":"no approval surface in this session; permission request denied automatically",
 "message":"Permission for this tool use was denied…","uuid":"…","session_id":"…"}
```

The synthesized `user` tool_result that follows has `is_error:true` and, at the
envelope level, `tool_result_meta:
[{"id":"toolu_01BdB2…","non_execution_kind":"permission-rule"}]` — the
machine-readable "this tool never ran" signal. The final `result` repeats it in
`permission_denials[]` with the full `tool_input`.

Surprising: `result.subtype` is still `"success"` and `is_error` is `false`
despite the denial. A run that was blocked does not look failed.

Also worth recording as a negative result: the first attempt at this fixture
asked for `echo hello` under `--permission-prompts none` and **was not denied** —
Claude Code's own safe-command classification auto-allowed it. Bash is not
reliably a prompting tool; `Write` is.

### `error.ndjson` — 7012 B, 7 lines

```
claude -p "Read src/does-not-exist.ts and summarise it." --max-turns 1 --allowed-tools "Read"
```

Two failure modes in one file: a failing tool call *and* a failed run.

```
… → assistant(tool_use:Read) → user(tool_result, is_error) → result/error_max_turns
```

* tool_result: `{"content":"File does not exist. Note: your current working
  directory is /FIXTURE_SANDBOX.","is_error":true,…}`; `tool_use_result` is the
  bare string `"Error: File does not exist…"`.
* `result`: `subtype:"error_max_turns"`, `is_error:true`, `terminal_reason:"max_turns"`,
  `stop_reason:"tool_use"`, and an **`errors` array** — `["Reached maximum number
  of turns (1)"]` — that the success shape does not have. The success-only keys
  `result`, `ttft_ms`, `ttft_stream_ms`, `time_to_request_ms`,
  `first_content_frame_ms` and `api_error_status` are **absent**. Process exit
  code was 1.
* `num_turns` is 2 even though `--max-turns 1`.

---

## Cross-cutting shapes

### `system` / `subtype: init`

```
type, subtype, cwd, session_id, tools[], mcp_servers[], model, permissionMode,
slash_commands[], terminal_slash_commands[], apiKeySource, claude_code_version,
output_style, agents[], skills[], plugins[{name,path,source,version?}],
capabilities[], analytics_disabled, product_feedback_disabled, uuid,
memory_paths{auto}, messaging_socket_path, fast_mode_state,
fast_mode_disabled_reason
```

Note `permissionMode` is the only camelCase key in the object. `model` is the
resolved id (`claude-sonnet-5`), not the alias. There is no `version` key —
it is `claude_code_version`. `messaging_socket_path` is host- and pid-specific.

### `assistant` with a `tool_use` block

```
type, message{ model, id, type, role, content[ {type:"tool_use", id, name,
  input{…typed…}, caller{type}} ], container, stop_reason, stop_sequence,
  stop_details, usage{ input_tokens, cache_creation_input_tokens,
  cache_read_input_tokens, cache_creation{ephemeral_5m_input_tokens,
  ephemeral_1h_input_tokens}, output_tokens, service_tier, inference_geo },
  diagnostics, context_management },
parent_tool_use_id, session_id, uuid, timestamp, request_id,
wire_tool_inputs{ <tool_use_id>: {…same input…} }
```

`parent_tool_use_id` is `null` at top level and is the subagent-correlation field
(see `--forward-subagent-text`). `timestamp` is ISO-8601 with millis.

### `user` with a `tool_result`

```
type, message{ role:"user", content[ {tool_use_id, type:"tool_result", content,
  is_error?} ] },
parent_tool_use_id, session_id, uuid, timestamp,
tool_use_result( object | string ),
tool_result_meta?[ {id, non_execution_kind} ]
```

`tool_use_result` is the structured payload and its shape is **per tool**:
`{type,file{filePath,content,numLines,startLine,totalLines}}` for `Read`,
`{stdout,stderr,interrupted,isImage,noOutputExpected}` for a successful `Bash`,
and a bare error **string** whenever the call failed. `is_error` is present on
some success results and absent on others — treat missing as false.

### `result`

```
type, subtype("success"|"error_max_turns"|…), is_error, session_id, uuid,
duration_ms, duration_api_ms, num_turns, stop_reason, terminal_reason,
total_cost_usd, usage{…}, modelUsage{ <model id>: {…} }, permission_denials[],
subagent_stats{…}, fast_mode_state, fast_mode_disabled_reason,
queued_turn_count, result_index,
  success only: result, ttft_ms, ttft_stream_ms, time_to_request_ms,
               first_content_frame_ms, api_error_status
  error only:   errors[]
```

### Where usage and cost actually live (§14)

Three places, and they do not agree:

1. **`result.total_cost_usd`** — the authoritative per-turn cost. This is what a
   `Run.costUsd` should come from.
2. **`result.usage`** — `input_tokens`, `output_tokens`,
   `cache_creation_input_tokens`, `cache_read_input_tokens`,
   `output_tokens_details.thinking_tokens`, `cache_creation.ephemeral_{5m,1h}_input_tokens`,
   `server_tool_use.{web_search_requests,web_fetch_requests}`, `service_tier`,
   `speed`, plus an `iterations[]` array of per-API-call breakdowns. This block
   covers **only the main model**.
3. **`result.modelUsage`** — keyed by full model id, camelCase field names
   (`inputTokens`, `outputTokens`, `cacheReadInputTokens`,
   `cacheCreationInputTokens`, `thinkingTokens`, `costUSD`, `contextWindow`,
   `maxOutputTokens`, `canonicalModel`, `provider`, `costBasis`). **This is the
   only place side-model spend shows up.** Every single fixture here contains a
   `claude-haiku-4-5-20251001` entry costing ~$0.001 that `result.usage` does not
   account for. `sum(modelUsage[*].costUSD) == total_cost_usd`.

So: `Run.costUsd` ← `total_cost_usd`; per-model breakdown ← `modelUsage`;
`Run.inputTokens/outputTokens/cachedTokens` ← `result.usage` if you want
main-model-only, or summed `modelUsage` if you want the truth. Do **not** sum
`assistant.message.usage` — see below.

### <a name="assistant-fan-out"></a>Assistant fan-out and double-counted usage

One API response is emitted as **one `assistant` line per content block**, all
sharing the same `message.id` and `request_id`, each with its own top-level
`uuid`, and each repeating an *identical* `message.usage` object. From
`tool-edit.ndjson`:

```
msg_011CeyoviNrNmjdaGSu7Y1g1  uuid 03255542  [thinking]   output_tokens=2
msg_011CeyoviNrNmjdaGSu7Y1g1  uuid 2664cf02  [tool_use]   output_tokens=2   <- same usage
msg_011CeyovwgJusQjyZTA1ZEoJ  uuid db85011c  [text]       output_tokens=1
msg_011CeyovwgJusQjyZTA1ZEoJ  uuid 34d0dacd  [tool_use]   output_tokens=1   <- same usage
msg_011Ceyow9fdY49Z74jMjQDHu  uuid 2bc1b976  [text]       output_tokens=2
```

Summing `usage` across `assistant` lines double-counts. Dedupe on `message.id`,
or just take the totals from `result`.

---

## Not captured

* **`permission.requested` via `--permission-prompt-tool`.** Needs the workspace
  MCP server from §6, which does not exist yet. `permission-denied.ndjson`
  captures the *deny* path via `--permission-prompts none` instead, which is the
  same tool_result/`permission_denials` plumbing minus the round trip to the
  host. The `system/permission_denied` shape recorded here is very likely what
  the approve/deny outcome looks like either way, but the *request* leg is
  unrecorded.
* **`workspace.report`** (§6 source 3 — `report_finding`, `open_plan`,
  `record_test_run`, `attach_artifact`, `attach_diagram`, `note_decision`).
  Needs the same MCP server. Nothing was built to fake it. On the wire these
  will be ordinary `assistant(tool_use:mcp__workspace__…)` /
  `user(tool_result)` pairs, so `tool-edit.ndjson` is a structural stand-in, but
  no real capture exists.
* **`process.output` / tier 3 (PTY).** Out of scope for stream-json.
* **Subagent traffic** (`--forward-subagent-text`, non-null
  `parent_tool_use_id`). No fixture spawns a `Task`. Worth adding next; the
  correlation field is present-but-null in every file here, which is exactly the
  case that hides bugs.
* **`--include-hook-events`.** Not recorded, though two hook messages leak into
  every fixture anyway from the recording machine's ambient `SessionStart` hook.
* **Interrupt / cancellation** (`interrupt_receipt_v1` is advertised in
  `init.capabilities`). No fixture interrupts a run.
* **`prompt_suggestion`** messages (`--prompt-suggestions`). Not recorded.
* **API-error results** (`api_error_status` non-null, rate-limit rejection,
  overload). Cannot be induced on demand honestly.

---

## Redaction

Applied to a copy; the raw captures stayed in a scratch directory outside this
repo.

| From | To |
|---|---|
| the absolute sandbox path | `/FIXTURE_SANDBOX` |
| that path with `/` → `-` (Claude Code's project-dir mangling, seen in `memory_paths`) | `-FIXTURE_SANDBOX` |
| `/home/<user>` | `/home/USER` |
| `-home-<user>` (mangled form) | `-home-USER` |

The redaction pass also scans for `sk-…` keys, `Bearer …`, `x-api-key`,
`gh[pousr]_…` tokens and email addresses. **Nothing matched** in any of the 13
files — no credential-shaped string was found or replaced. The sandbox's git
identity is the deliberately fake `fixtures@example.com`, which is allowlisted.

Everything else is byte-identical: session uuids, message uuids, `msg_`/`req_`/
`toolu_` ids, ISO timestamps, durations, token counts and dollar costs all
survive, because the ordering/dedupe/metrics tests need them.

Two machine-specific values are **intentionally left in**, since they are
neither secret nor reconstructible: `init.messaging_socket_path`
(`/run/user/1000/cc-socks/<pid>.sock`) and the `plugins` / `skills` /
`slash_commands` lists.

---

## Reproducing

```bash
./record.sh                 # all scenarios
./record.sh tool-edit       # one scenario
```

`record.sh` builds its own sandbox under a `mktemp -d`, refuses to run if the
sandbox would land inside this repo, writes raw captures to a scratch dir, and
only ever writes redacted `.ndjson` files next to itself. Overrides:
`CLAUDE_BIN`, `SANDBOX_DIR`, `RAW_DIR`, `WORK`, `MODEL`, `RUN_TIMEOUT`.

Re-record against every new Claude Code version and diff the resulting
`type`/`subtype` sequences — that diff is the §16 drift alarm.
