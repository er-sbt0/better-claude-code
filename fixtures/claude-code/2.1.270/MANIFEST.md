# Fixtures — Claude Code 2.1.270

**Complete.** All thirteen scenarios from the 2.1.269 set, plus `workspace-mcp`,
which is new here because the workspace MCP server did not exist when 2.1.269
was captured.

Recorded 2026-09-13 against `claude 2.1.270` by `./record.sh`, redacted by the
same pass as the 2.1.269 set. Raw captures were kept out of the repository.

`2.1.269/MANIFEST.md` documents what every scenario is *for* and every provider
message shape in detail. **None of that changed**, so this file does not repeat
it. What follows is only the delta.

| Scenario | 2.1.269 | 2.1.270 |
|---|---|---|
| `error` | 7 | 7 |
| `multi-turn` | 11 | 11 |
| `partial-messages` | 13 | 13 |
| `permission-denied` | 9 | 9 |
| `resume-1` / `resume-2` | 6 / 6 | 6 / 6 |
| `text-only` | 7 | 7 |
| `thinking` | 52 | 25 |
| `thinking-partial` | 72 | 84 |
| `tool-bash` | 8 | 8 |
| `tool-bash-fail` | 12 | 15 |
| `tool-edit` | 12 | 12 |
| `tool-read` | 9 | 8 |
| `workspace-mcp` | — | 18 |

## The drift answer: none

§16's reason for keeping fixtures per version is that the diff between them is
the alarm. It did not go off.

**The provider's message vocabulary is byte-identical across the two versions.**
The same twelve `type`/`subtype` combinations appear and no others:

```
assistant            result/error_max_turns   system/init                user
rate_limit_event     result/success           system/permission_denied
stream_event         system/hook_response     system/status
                     system/hook_started      system/thinking_tokens
```

**Every scenario has exactly 2 unclassified events in both versions** — the two
leading `system/hook_*` messages that the 2.1.269 MANIFEST already attributes to
ambient user settings. Nothing new leaked into `raw.unclassified`, which is the
signal that would have meant a message type we do not model.

The normalizer required no change to read any of these streams.

## What did differ, and why none of it is drift

Four goldens differ; all four are the model behaving differently on the same
prompt, not the provider emitting a different shape.

- **`tool-read`** lost an `assistant.thinking` block and **`tool-bash-fail`**
  gained one. Opposite directions on the same recording run, which is what
  variance looks like. Line counts moved with them.
- **`thinking`** has the same event sequence exactly; only the count of
  `system/thinking_tokens` messages changed (45 → 18). Those are transient and
  dropped before they reach the log (§8), so this is invisible downstream.
- **`text-only`** emits `rate_limit_event` one position earlier, before the
  replayed `user` message rather than after it. Worth knowing that its position
  is not stable, since anything that keyed off the ordering of a non-content
  message would be relying on an accident.

The lesson the 2.1.269 MANIFEST already drew — that these captures contain real
model output and are therefore only stable in *shape* — holds. Assert on the
type sequence and the field shapes; never on counts of thinking blocks.

## `workspace-mcp`

The one scenario with no 2.1.269 counterpart. It is the only one that needs a
second process alive for the duration, because its tools exist only if our MCP
server is running: `crates/workspace-mcp/examples/host` binds the socket and
writes the flags it implies to a file, `record.sh` sources them and invokes
`claude` exactly as it does everything else. The driver is deliberately not
involved — consuming stdout to produce events is the one thing a recorder must
not do. The host exits non-zero if no permission request ever reached it, so a
capture from a run that never talked to the server fails the recording rather
than quietly looking like evidence.

```
 3 system/init      mcp_servers=['agent-workspace']
 8 assistant        tool_use:Edit
 9 user             tool_result                     <- succeeded, having been permitted
12 assistant        tool_use:mcp__agent-workspace__report_finding
13 user             tool_result                     <- is_error: malformed arguments
15 assistant        tool_use:mcp__agent-workspace__report_finding
16 user             tool_result                     <- accepted
```

**The 2.1.269 MANIFEST's prediction held.** A workspace call is an ordinary
`assistant(tool_use)` / `user(tool_result)` pair under the
`mcp__agent-workspace__` prefix. Note the server name: `workspace` is reserved
by Claude Code and a config claiming it is dropped with a debug-channel warning,
so a run whose `init` does not list `agent-workspace` is a run whose config was
rejected.

**The model's first `report_finding` call was malformed.** Its input arrives as
`{"__unparsedToolInput": …}` — the provider's marker for arguments it could not
parse — and the *only* record that it failed is `is_error` on the completion.
This is why `workspace.report` replaces `tool.started` but does not suppress the
`tool.completed` answering it: the report says what the agent meant to tell us,
the completion says whether it managed to. A model that gets the schema wrong
twice would otherwise look like one that reported nothing.

That it then retried correctly is not something to rely on.

## What no fixture in any version can contain

**`permission.requested`.** The `Edit` in `workspace-mcp.ndjson` succeeded,
which means a permission request was asked and answered — and nothing about it
appears anywhere in the stream, because the provider's permission layer calls
our tool directly rather than the model calling it. Replaying a recorded stream
can no more produce a `permission.requested` than it can produce `run.finished`.

So §16's defense does not extend to it. That contract is guarded by
`docs/verification/claude-code-permission-prompt-tool.md`, re-read against the
binary, and by a driver test. A change to it surfaces as runs failing, never as
a golden diff.
