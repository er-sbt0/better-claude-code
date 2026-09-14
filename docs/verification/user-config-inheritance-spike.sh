#!/usr/bin/env bash
#
# What does a run launched by our driver inherit from the user's own machine?
#
# §5's "What users keep" list claims credentials, CLI installation,
# configuration, MCP servers, skills, permissions and project setup. The driver
# passes `--strict-mcp-config` unconditionally, which cannot be true of MCP
# servers. Nobody had measured which of the other six are true, so the shape of
# the exception -- one item, or six -- was a guess.
#
# This runs the driver's exact argv (`LaunchSpec::argv`) against a fixture
# environment we own end to end, once with `--strict-mcp-config` and once
# without, and diffs what the agent can see.
#
# Most of it costs nothing: hooks fire at SessionStart before a prompt arrives,
# and the `system/init` message enumerates servers, tools, skills, agents,
# commands, plugins and the output style. Only the CLAUDE.md check needs the
# model -- two haiku turns, well under a cent. Asserts rather than prints:
# exits non-zero on any regression.
#
#   bash docs/verification/user-config-inheritance-spike.sh
#
# Env: CLAUDE_BIN (default: claude), MODEL (default: claude-haiku-4-5-20251001)
#
# **It never touches the real ~/.claude.** The user scope is an isolated HOME
# built from scratch under $WORK; the one thing borrowed from the real home is
# `.credentials.json`, by *symlink* rather than copy, because there is no other
# way to authenticate a run and a copied credential is a secret in two places.
# Every other file the CLI reads -- settings, CLAUDE.md, skills, agents,
# plugins, `.claude.json` -- is ours.

set -uo pipefail

CLAUDE_BIN="${CLAUDE_BIN:-claude}"
MODEL="${MODEL:-claude-haiku-4-5-20251001}"
WORK="$(mktemp -d -t aw-inherit-XXXXXX)"
HOMEDIR="$WORK/home"
REPO="$WORK/repo"
HOOKS="$WORK/hooks-fired.txt"
SERVERS="$WORK/servers-started.txt"

fail=0
ok()   { printf '  ok       %s\n' "$1"; }
bad()  { printf '  FAIL     %s\n' "$1"; fail=1; }
check(){ if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (want '$3', got '$2')"; fi }
has()  { case "$2" in *"$3"*) ok "$1";; *) bad "$1 (want '$3' in '$2')";; esac }
hasnt(){ case "$2" in *"$3"*) bad "$1 (did not want '$3' in '$2')";; *) ok "$1";; esac }

command -v "$CLAUDE_BIN" >/dev/null || { echo "no claude on PATH" >&2; exit 1; }
[ -f "$HOME/.claude/.credentials.json" ] || {
  echo "no ~/.claude/.credentials.json to borrow; this spike cannot authenticate" >&2; exit 1; }
echo "claude: $($CLAUDE_BIN --version)"
echo "work:   $WORK"

# ---------------------------------------------------------------- the fixture

mkdir -p "$HOMEDIR/.claude/skills/userskill" "$HOMEDIR/.claude/agents" \
         "$HOMEDIR/.claude/output-styles" \
         "$REPO/.claude/skills/projskill" "$REPO/.claude/agents" "$REPO/.claude/commands" \
         "$WORK/market/.claude-plugin" "$WORK/market/spikeplug/.claude-plugin" \
         "$WORK/market/spikeplug/commands" "$WORK/market/spikeplug/skills/plugskill"
ln -s "$HOME/.claude/.credentials.json" "$HOMEDIR/.claude/.credentials.json"

# An MCP server that connects for real, so a loaded one shows as `connected`
# with a tool of its own rather than as a failure we would have to interpret.
# It also records that it was *started*, which is the half `init` cannot fake.
cat > "$WORK/server.py" <<'PY'
#!/usr/bin/env python3
import sys, json, os
name = sys.argv[1]
open(os.environ["WITNESS"], "a").write(name + "\n")
def send(o): sys.stdout.write(json.dumps(o) + "\n"); sys.stdout.flush()
for line in sys.stdin:
    try: m = json.loads(line)
    except Exception: continue
    if "id" not in m: continue
    if m.get("method") == "initialize":
        send({"jsonrpc":"2.0","id":m["id"],"result":{"protocolVersion":"2024-11-05",
             "capabilities":{"tools":{}},"serverInfo":{"name":name,"version":"0.0.1"}}})
    elif m.get("method") == "tools/list":
        send({"jsonrpc":"2.0","id":m["id"],"result":{"tools":[{"name":"probe_"+name,
             "description":"spike probe","inputSchema":{"type":"object","properties":{}}}]}})
    else:
        send({"jsonrpc":"2.0","id":m["id"],"result":{}})
PY
chmod +x "$WORK/server.py"
srv() { printf '{"command":"%s","args":["%s"],"env":{"WITNESS":"%s"}}' "$WORK/server.py" "$1" "$SERVERS"; }

# user scope (~/.claude.json mcpServers) and local scope (projects.<cwd>)
cat > "$HOMEDIR/.claude.json" <<EOF
{ "hasCompletedOnboarding": true,
  "mcpServers": { "userscope": $(srv userscope) },
  "projects": { "$REPO": { "hasTrustDialogAccepted": true,
                           "mcpServers": { "localscope": $(srv localscope) } } } }
EOF
# project scope (.mcp.json), auto-approved so the control is conclusive
echo "{ \"mcpServers\": { \"projectscope\": $(srv projectscope) } }" > "$REPO/.mcp.json"

hook() { printf '{"SessionStart":[{"hooks":[{"type":"command","command":"echo %s >> %s"}]}]}' "$1" "$HOOKS"; }
cat > "$HOMEDIR/.claude/settings.json" <<EOF
{ "hooks": $(hook user), "env": { "SPIKE_USER_ENV": "user-env-value" },
  "outputStyle": "spikestyle" }
EOF
cat > "$REPO/.claude/settings.json" <<EOF
{ "hooks": $(hook project), "env": { "SPIKE_PROJECT_ENV": "project-env-value" },
  "enableAllProjectMcpServers": true }
EOF
echo "{ \"hooks\": $(hook local) }" > "$REPO/.claude/settings.local.json"

echo 'The user-scope codeword is MARKER-USER-7731.'    > "$HOMEDIR/.claude/CLAUDE.md"
echo 'The project-scope codeword is MARKER-PROJECT-4402.' > "$REPO/CLAUDE.md"
fm() { printf -- '---\nname: %s\ndescription: %s\n---\nbody\n' "$1" "$2"; }
fm userskill "spike user skill"       > "$HOMEDIR/.claude/skills/userskill/SKILL.md"
fm projskill "spike project skill"    > "$REPO/.claude/skills/projskill/SKILL.md"
fm useragent "spike user agent"       > "$HOMEDIR/.claude/agents/useragent.md"
fm projagent "spike project agent"    > "$REPO/.claude/agents/projagent.md"
fm spikestyle "spike output style"    > "$HOMEDIR/.claude/output-styles/spikestyle.md"
printf -- '---\ndescription: spike command\n---\nhello\n' > "$REPO/.claude/commands/spikecmd.md"

# A plugin contributes a command, a skill *and* an MCP server, which is the
# only fixture that can show the flag cutting one plugin in half.
cat > "$WORK/market/.claude-plugin/marketplace.json" <<EOF
{ "name": "spikemarket", "owner": {"name":"spike"},
  "plugins": [ {"name":"spikeplug","source":"./spikeplug","description":"spike"} ] }
EOF
cat > "$WORK/market/spikeplug/.claude-plugin/plugin.json" <<EOF
{ "name": "spikeplug", "version": "0.0.1", "description": "spike",
  "mcpServers": { "plugscope": $(srv plugscope) } }
EOF
printf -- '---\ndescription: spike plugin command\n---\nhi\n' > "$WORK/market/spikeplug/commands/plugcmd.md"
fm plugskill "spike plugin skill" > "$WORK/market/spikeplug/skills/plugskill/SKILL.md"

git -C "$REPO" init -q -b main
git -C "$REPO" config user.email "fixtures@example.com"
git -C "$REPO" config user.name "Fixture Recorder"
git -C "$REPO" add -A && git -C "$REPO" commit -qm init

# `env -i`, because this spike is usually run from inside a Claude Code session
# and the CLAUDECODE/CLAUDE_CODE_* variables it exports change what a child
# `claude` believes about itself. The driver spawns from a desktop app, where
# none of them are set.
cli() { env -i HOME="$HOMEDIR" PATH="$PATH" TERM=dumb LANG=C.UTF-8 \
            USER="${USER:-user}" SHELL=/bin/bash "$CLAUDE_BIN" "$@"; }

# The marketplace shape in settings.json is undocumented and a wrong guess is
# swallowed silently -- an invalid `extraKnownMarketplaces` makes the CLI drop
# the *whole* user settings file with nothing on any channel. Let the CLI write
# it.
( cd "$REPO" && cli plugin marketplace add "$WORK/market" >/dev/null 2>&1 )
( cd "$REPO" && cli plugin install spikeplug@spikemarket >/dev/null 2>&1 )

# ------------------------------------------------------------------ the runs

uuid() { python3 -c 'import uuid;print(uuid.uuid4())'; }
msg()  { python3 -c 'import json,sys;print(json.dumps({"type":"user","message":{"role":"user","content":[{"type":"text","text":sys.argv[1]}]},"parent_tool_use_id":None}))' "$1"; }

# run <label> <strict|loose> [extra flags...] -- stdin is the prompt, or
# /dev/null for a config-only probe. This is `LaunchSpec::argv` verbatim, minus
# the flags that are not about configuration.
run() {
  local label="$1" mode="$2"; shift 2
  local -a args=(--output-format stream-json --verbose --input-format stream-json
                 --replay-user-messages)
  [ "$mode" = strict ] && args+=(--strict-mcp-config)
  args+=(--session-id "$(uuid)" --thinking-display summarized --model "$MODEL"
         --permission-prompts none --max-turns 3 --max-budget-usd 0.10 "$@" -p)
  : > "$HOOKS"; : > "$SERVERS"
  ( cd "$REPO" && cli "${args[@]}" ) > "$WORK/$label.out" 2> "$WORK/$label.err"
  HOOKS_FIRED="$(sort -u "$HOOKS" | tr '\n' ' ' | sed 's/ $//')"
  SERVERS_STARTED="$(sort -u "$SERVERS" | tr '\n' ' ' | sed 's/ $//')"
}

# field <label> <init key or "result"> -- one line, sorted, comparable.
field() {
  python3 - "$WORK/$1.out" "$2" <<'PY'
import json, sys
path, key = sys.argv[1], sys.argv[2]
for line in open(path):
    try: d = json.loads(line)
    except Exception: continue
    if key == "result":
        if d.get("type") == "result": print((d.get("result") or "").strip()); break
    elif d.get("subtype") == "init":
        v = d.get(key)
        if key == "mcp_servers": v = sorted(s["name"] for s in v)
        elif key == "tools":     v = sorted(t for t in v if t.startswith("mcp__"))
        elif key in ("skills", "slash_commands", "agents"):
            v = sorted(x for x in v if x in ("userskill", "projskill", "spikeplug:plugskill",
                                             "spikecmd", "spikeplug:plugcmd",
                                             "useragent", "projagent"))
        elif key == "plugins": v = sorted(p["name"] for p in v)
        print(json.dumps(v)); break
PY
}

# A prompt goes in through a file, never a pipe: `x | run` puts `run` in a
# subshell and its `HOOKS_FIRED` would never come back.
msg 'Reply with one line and nothing else: the project-scope codeword and the user-scope codeword that appear in your instructions, or NONE for each you cannot see.' > "$WORK/codewords.json"
msg 'reply with the single word ok' > "$WORK/trivial.json"

echo
echo "== the control: the same argv without --strict-mcp-config"
run loose loose < "$WORK/codewords.json"
LOOSE_HOOKS="$HOOKS_FIRED"; LOOSE_SERVERS="$SERVERS_STARTED"
check "user, project and local hooks all fire" "$LOOSE_HOOKS" "local project user"
check "all four MCP scopes start" "$LOOSE_SERVERS" "localscope plugscope projectscope userscope"
has   "user CLAUDE.md is in context"    "$(field loose result)" "MARKER-USER-7731"
has   "project CLAUDE.md is in context" "$(field loose result)" "MARKER-PROJECT-4402"

echo
echo "== the driver's argv, with --strict-mcp-config"
run strict strict < "$WORK/codewords.json"
check "hooks are unchanged"          "$HOOKS_FIRED" "$LOOSE_HOOKS"
check "no MCP server starts at all"  "$SERVERS_STARTED" ""
check "init lists no servers"        "$(field strict mcp_servers)" "[]"
check "and no mcp__ tool"            "$(field strict tools)" "[]"
has   "user CLAUDE.md still applies"    "$(field strict result)" "MARKER-USER-7731"
has   "project CLAUDE.md still applies" "$(field strict result)" "MARKER-PROJECT-4402"

echo
echo "== the flag touches MCP and nothing else"
for k in skills slash_commands agents plugins output_style; do
  check "init.$k is identical" "$(field strict $k)" "$(field loose $k)"
done

echo
echo "== the parts of the user's configuration that are not MCP"
# Both scopes' `env` blocks, read back out of the environment a hook runs in.
printf '{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"echo u=$SPIKE_USER_ENV p=$SPIKE_PROJECT_ENV >> %s"}]}]}}' "$HOOKS" > "$WORK/envhook.json"
run env strict --settings "$WORK/envhook.json" < /dev/null
has "settings env from the user scope is exported"    "$HOOKS_FIRED" "u=user-env-value"
has "settings env from the project scope is exported" "$HOOKS_FIRED" "p=project-env-value"

run merged strict --settings "{\"hooks\":$(hook viaflag)}" < /dev/null
check "--settings merges with the user's files, it does not replace them" \
      "$HOOKS_FIRED" "local project user viaflag"

# A `permissions` block in the *user's own* settings file, not on the command
# line: a denied tool is gone from `init.tools` entirely, which is why this
# needs a prompt and the checks above do not.
python3 - "$HOMEDIR/.claude/settings.json" <<'PY'
import json, sys
p = sys.argv[1]; d = json.load(open(p)); d["permissions"] = {"deny": ["Bash"]}
json.dump(d, open(p, "w"))
PY
run denied strict < "$WORK/trivial.json"
hasnt "a deny in the user's settings removes the tool from the run" \
      "$(python3 -c 'import json,sys
for l in open(sys.argv[1]):
    d=json.loads(l)
    if d.get("subtype")=="init": print(json.dumps(d["tools"])); break' "$WORK/denied.out")" '"Bash"'
python3 - "$HOMEDIR/.claude/settings.json" <<'PY'
import json, sys
p = sys.argv[1]; d = json.load(open(p)); d.pop("permissions", None)
json.dump(d, open(p, "w"))
PY

echo
echo "== the knobs the driver does not pass"
run scoped strict --setting-sources project < /dev/null
check "--setting-sources selects scopes" "$HOOKS_FIRED" "project"
run bare strict --bare < /dev/null
check "--bare skips hooks entirely" "$HOOKS_FIRED" ""
run restricted strict --restricted < /dev/null
check "--restricted ignores user, project and local settings" "$HOOKS_FIRED" ""

echo
if [ "$fail" = 0 ]; then echo "all checks passed"; else echo "FAILURES -- see above"; fi
echo "artifacts in $WORK"
exit "$fail"
