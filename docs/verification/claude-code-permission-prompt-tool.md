<!--
Verification record. Discharges the "two event classes need a workspace MCP
server that does not exist" debt in docs/roadmap.md §7, for the parts that are
the *provider's* contract rather than ours: how an MCP server is injected, and
what the permission-prompt tool must return.

Verified: 2026-09-13 · Claude Code 2.1.270 · Ubuntu 25.04, x86-64
Method: `claude --help`, hidden option definitions and zod schemas extracted
        from the binary, cross-checked against live validation errors, then
        exercised end to end by `crates/workspace-mcp` against a real run.
-->

# Claude Code: MCP injection and the permission-prompt tool

`docs/verification/claude-code-cli-2.1.270.md` verified that `--mcp-config`,
`--strict-mcp-config` and `--permission-prompt-tool` exist and how they are
spelled. It did not record two things the workspace MCP server cannot be built
without:

1. the JSON schema `--mcp-config` accepts, and
2. what the permission-prompt tool must **return** — the response contract.

Both are below. §16's rule applies to them as much as to the flags — they change
without semver — but its *mechanism* does not: neither contract appears in a
recorded stream, so replaying fixtures cannot catch a change to either. Only
re-reading the binary can, which is what makes this file load-bearing rather
than a convenience.

**Binary:** `/home/eransa/.local/bin/claude` → `~/.local/share/claude/versions/2.1.270`.

Every flag claim in the 2.1.269 record reproduces verbatim on 2.1.270. Nothing
in that document needed correcting; this one only adds.

## 1. The MCP config schema

`--mcp-config <configs...>` is variadic and takes **either** a file path **or**
inline JSON. The discriminator is textual, not positional — a value whose
trimmed text starts with `{` is parsed as JSON, anything else is resolved as a
path. `--settings` uses the same rule.

That is why nothing in this repository writes a temp file: the whole server
config goes on the command line.

The root object must have an `mcpServers` key mapping server name to config:

```json
{"mcpServers": {"agent-workspace": {"type": "stdio", "command": "/abs/path/relay", "args": [], "env": {"K": "v"}}}}
```

### Reserved server names — the one that cost a run to find

**`workspace` is reserved** for Claude Code's own hosted tools
(`mcp__workspace__bash`, `mcp__workspace__web_fetch`), along with `stdio`,
`claude_ai`, `computer-use`, `claude-in-chrome`, `hearthbot` and `__proto__`.
A config claiming one is **not** an error:

```
--mcp-config: 1 entry warning(s): mcpServers.workspace:
  "workspace" is a reserved MCP server name and was not loaded
```

That warning goes to the debug channel. The run then starts normally with no
server. With `--permission-prompt-tool` the only symptom is a startup message
blaming the tool — `MCP tool mcp__workspace__request_permission ... not found.
Available MCP tools: none` — and without it, the agent is simply and silently
tool-less.

Nothing in `--help`, in the config errors, or in the schema mentions this. It
was found by running the binary. Our server is therefore named
**`agent-workspace`**, which is what §6 and the older launch-shape record should
be read as saying wherever they spell it `workspace`.

Getting the root key wrong is a named error rather than a silent no-op, which
is worth knowing because it is the failure a hand-built config actually hits:

```
Missing "mcpServers" — found "servers" instead. Claude Code reads MCP servers from the "mcpServers" key.
```

Server variants and their required fields, from the schema in the binary:

| `type` | required | notes |
|---|---|---|
| `stdio` | `command` (non-empty) | **`type` is optional here and only here**; `args` defaults to `[]`; `env` is a string→string map |
| `http` | `type`, `url` | `headers` optional |
| `sse` | `type`, `url` | `headers`, `oauth`, `toolPermissions` optional |
| `ws` | `type`, `url` | |
| `sdk` | `type`, `name` | in-process; not reachable from a subprocess driver |

`env` on the stdio variant is the load-bearing one for us. The driver spawns
`claude` with no environment of its own (`crates/driver/src/process.rs` sets
`current_dir` and three pipes, nothing else), so this map is the **only**
channel that reaches the server process. It is how the relay learns its socket
path.

One spelling trap: inside the JSON the key is `type`. `transport` is the
spelling of the *CLI* flag on `claude mcp add -t`, and the binary rejects it in
JSON with a message that says so explicitly.

## 2. `--permission-prompt-tool`

Hidden from `--help` since 2.1.269, still registered, still `--print`-only:

```js
.addOption(new z("--permission-prompt-tool <tool>","MCP tool to use for permission prompts (only works with --print)").argParser(String).hideHelp())
```

**The name must be fully qualified.** The CLI splits on `__` and requires the
first segment to be literally `mcp`:

```js
function xs(e){let n=e.split("__"),[t,r,...o]=n;
  if(t!=="mcp"||!r)return null;
  let s=o.length>0?o.join("__"):undefined;
  return{serverName:r,toolName:s}}
```

A bare `request_permission` parses to `null` and cannot resolve. The server name
recovered here is not cosmetic: the CLI uses it to wait for that specific MCP
server during startup, so a config that fails to load the named server makes
every permission request fail rather than hang.

The literal value `stdio` is reserved — the SDK passes it for its in-process
`canUseTool` path. Do not name a server `stdio`.

### The request

The tool is called with:

```json
{"tool_name": "Write", "input": {"file_path": "...", "content": "..."}, "tool_use_id": "toolu_..."}
```

Note that this call is made by the CLI's own permission layer, **not by the
model**. It is therefore not a `tool_use` block on the wire, and no amount of
reading the stream will produce it — which is precisely why §15 can require that
permission requests come from the runtime's own channel and mean something by
it. Our server observing this call *is* that channel.

### The response

This is the part that was asserted and is now verified. The result must be
**exactly one content block** of `type: "text"`, whose `text` is a **JSON
string** — not a JSON object. A nested encoding, and easy to get wrong:

```json
{"content": [{"type": "text", "text": "{\"behavior\":\"allow\"}"}], "isError": false}
```

Anything else is rejected with:

```
Permission prompt tool returned an invalid result. Expected a single text block param with type="text" and a string text value.
```

The decoded text must match one of two shapes:

```json
{"behavior":"allow","updatedInput":{...}?,"updatedPermissions":[...]?,"toolUseID":"..."?,"decisionClassification":"..."?}
{"behavior":"deny","message":"...","interrupt":bool?,"toolUseID":"..."?,"decisionClassification":"..."?}
```

- **`message` is required on deny.** It is optional in the `PermissionRequest`
  *hook*'s otherwise-identical shape, which is a good way to be misled.
- `updatedInput`, when present and non-empty, **replaces** the tool's input and
  is validated against that tool's own schema; a mismatch fails the call with
  `permission_updated_input_invalid` rather than falling back to the original.
  Omit it to accept the input unchanged.
- `decisionClassification` is `user_temporary | user_permanent | user_reject`.
  Unset, the CLI infers conservatively — temporary for allow, reject for deny.
  A workspace that wants "allow for this workspace" to persist has to say so.
- `interrupt: true` on a deny aborts the turn rather than returning an error to
  the model.

**The casing flips between request and response.** It is `tool_use_id` going in
and `toolUseID` coming back. Both spellings are correct; making them consistent
breaks one direction.

## 3. MCP tool names in permission rules

`--allowed-tools` and `--disallowed-tools` accept MCP tools, but **not** the
parenthesis patterns that work for built-ins:

```
MCP rules do not support patterns in parentheses
Use "<name>" without parentheses, or use "mcp__<server>__*" for all tools
```

Three valid spellings: `mcp__<server>` (the whole server), `mcp__<server>__*`
(all its tools), `mcp__<server>__<tool>` (one tool).

## 4. Confirmed by running it

Two live runs through `crates/driver` against 2.1.270, one allowing and one
denying:

- An **allow** reaches the tool and the `Edit` actually lands on disk; a
  **deny** is honoured, the tool result comes back `is_error` with
  `non_execution: permission-rule`, and the provider's own `permission_denials`
  count for the turn agrees with ours.
- The MANIFEST's prediction for `workspace.report` **held**: it arrives as an
  ordinary `assistant(tool_use: mcp__agent-workspace__report_finding)` /
  `user(tool_result)` pair.
- **The reporting tool is itself permission-gated.** The first run raised a
  `request_permission` *for* `mcp__agent-workspace__report_finding`, so a
  deny-by-default policy silently denies its own reporting channel. The driver
  allow-lists that one tool and deliberately does not allow-list
  `request_permission`, which the model has no business calling.

## 5. What this does not cover
- **A recorded fixture.** `workspace.report` is on the wire and could have one;
  `permission.requested` structurally cannot, because it never appears on
  stdout. §16's replay ritual therefore does not protect this contract — only
  re-reading the binary does, which is what this file is for.
- **`updatedInput` and `updatedPermissions` in practice.** The schemas are read
  from the binary; the workspace sends neither yet, so neither has been
  exercised against a real run.
- **`decisionClassification`.** Same — the shape is verified, the behaviour is
  not. It is what "allow for this workspace" in §12's V1 surface will need.
- **Anything but stdio.** The http/sse/ws variants are recorded above for
  completeness and are untried.
