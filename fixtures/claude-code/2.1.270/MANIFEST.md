# Fixtures — Claude Code 2.1.270

**Partial.** One scenario, recorded because the workspace MCP server did not
exist when the 2.1.269 set was captured and its two event classes were the
largest asserted shapes in the protocol (design §16).

The other twelve scenarios are **not** recorded here. `record.sh` in this
directory is the 2.1.269 script plus the new scenario, so completing the set is
`./record.sh` with no arguments — that is the "re-record against 2.1.270" item
the roadmap still owes. Until then, `2.1.269/` remains the baseline the named
regression tests in `crates/normalizer/tests/fixtures.rs` assert against.

| Scenario | Lines | What it is evidence of |
|---|---|---|
| `workspace-mcp` | 18 | a workspace MCP tool call on the wire, both malformed and well-formed |

Recorded 2026-09-13 against `claude 2.1.270`, redacted by the same pass as the
2.1.269 set. Raw captures were kept out of the repository.

## How this one is recorded, and why it is different

Every other fixture is the raw stdout of one `claude` process with nothing of
ours in the pipe. This one still is — but it needs a second process alive for
the duration, because the tools only exist if our MCP server is running.

`crates/workspace-mcp/examples/host` binds the socket and writes the flags it
implies to a file; `record.sh` sources them and invokes `claude` exactly as it
does for every other scenario. The driver is deliberately not involved: it
consumes stdout to produce events, which is the one thing a fixture recorder
must not do.

The host exits non-zero if no permission request ever reached it, so a capture
from a run that never actually talked to the server fails the recording instead
of quietly looking like evidence.

## What the capture shows

```
 3 system/init      mcp_servers=['agent-workspace']
 8 assistant        tool_use:Edit
 9 user             tool_result                     <- succeeded, having been permitted
12 assistant        tool_use:mcp__agent-workspace__report_finding
13 user             tool_result                     <- is_error: malformed arguments
15 assistant        tool_use:mcp__agent-workspace__report_finding
16 user             tool_result                     <- accepted
```

**The MANIFEST's prediction for `workspace.report` held.** A workspace tool call
is an ordinary `assistant(tool_use)` / `user(tool_result)` pair under the
`mcp__agent-workspace__` prefix, exactly as the 2.1.269 set guessed it would be.

**The first `report_finding` call was malformed.** Its input arrives as
`{"__unparsedToolInput": …}` — the provider's marker for arguments it could not
parse as JSON — and the *only* record that it failed is `is_error` on the
completion. This is why `workspace.report` replaces `tool.started` but does not
suppress the `tool.completed` that answers it: the report says what the agent
meant to tell us, and the completion says whether it managed to. A run where the
model gets the schema wrong twice would otherwise look like a run that reported
nothing.

That the model then retried correctly is not something to rely on.

## What no fixture in any version can contain

**`permission.requested`.** It is not in this capture and it never will be in
one. The `Edit` at line 8 succeeded, which means a permission request was asked
and answered — and nothing about that appears on stdout, because the provider's
permission layer calls our tool directly rather than the model calling it.
Replaying a recorded stream can no more produce a `permission.requested` than it
can produce `run.finished`.

So §16's usual defense does not apply to it. The permission-prompt tool's
contract is guarded by `docs/verification/claude-code-permission-prompt-tool.md`
— re-read against the binary — and by a driver test, not by this directory. A
change to it will surface as runs failing, never as a golden diff.

## Drift against 2.1.269

Nothing in the shared surface changed. `system/init`, `assistant`, `user`,
`result/success` and the `rate_limit_event` all carry the shapes the 2.1.269
MANIFEST documents, and the two leading `system/hook_*` messages are the same
ambient-settings leakage noted there. The normalizer needed no change to read
this stream beyond the workspace-report branch it was written for.

Two things this capture adds to the 2.1.269 notes:

- **`mcp_servers` in `init` carries our server**, as `agent-workspace`. A run
  where it is absent is a run whose config was rejected — which, for a reserved
  name, happens with only a debug-channel warning.
- **`ToolSearch` appears** (line 10) as the model's way of locating a tool it
  was told about but had not been given the schema for. Harmless, and not
  something any 2.1.269 fixture shows.
