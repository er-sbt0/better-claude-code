//! The wire contract, as a living guard.
//!
//! `docs/verification/claude-code-permission-prompt-tool.md` is the dated
//! record of what Claude Code 2.1.270 accepts and rejects. These are the same
//! claims as assertions, so a change to this crate that would make the CLI
//! answer "Permission prompt tool returned an invalid result" breaks a test
//! here instead of breaking a run — a failure that is otherwise invisible
//! until an agent asks for permission and silently cannot get it.
//!
//! Almost everything is asserted against [`Server::handle_line`] rather than
//! through a socket, because the socket adds no protocol. The one round trip
//! over a real unix socket exists to prove that.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use workspace_mcp::{
    qualified, Decision, Listener, PermissionRequest, Server, Workspace, REPORT_FINDING,
    REQUEST_PERMISSION, SERVER_NAME, SOCKET_ENV,
};

// ------------------------------------------------------------------ fixture --

/// A workspace that answers with whatever it was handed and remembers
/// everything it was asked.
struct Stub {
    decision: Decision,
    permissions: Mutex<Vec<PermissionRequest>>,
    reports: Mutex<Vec<(String, Value)>>,
}

impl Stub {
    fn new(decision: Decision) -> Arc<Stub> {
        Arc::new(Stub {
            decision,
            permissions: Mutex::new(Vec::new()),
            reports: Mutex::new(Vec::new()),
        })
    }

    fn allowing() -> Arc<Stub> {
        Stub::new(Decision::Allow {
            updated_input: None,
        })
    }
}

impl Workspace for Stub {
    fn request_permission(&self, req: PermissionRequest) -> Decision {
        self.permissions.lock().unwrap().push(req);
        self.decision.clone()
    }

    fn report(&self, tool: &str, payload: Value) {
        self.reports
            .lock()
            .unwrap()
            .push((tool.to_owned(), payload));
    }
}

fn server(stub: &Arc<Stub>) -> Server {
    Server::new(Arc::clone(stub) as Arc<dyn Workspace>)
}

/// Send one line and parse the response, insisting there was one.
fn call(server: &Server, request: Value) -> Value {
    let line = server
        .handle_line(&request.to_string())
        .expect("a request with an id is always answered");
    let response: Value = serde_json::from_str(&line).expect("the response is one JSON object");
    assert_eq!(response["jsonrpc"], "2.0", "in {line}");
    assert_eq!(response["id"], request["id"], "in {line}");
    response
}

/// The `result` of a successful call.
fn ok(server: &Server, request: Value) -> Value {
    let response = call(server, request);
    assert!(
        response.get("error").is_none(),
        "expected a result, got {response}"
    );
    response["result"].clone()
}

fn tools_call(server: &Server, name: &str, arguments: Value) -> Value {
    ok(
        server,
        json!({"jsonrpc":"2.0","id":7,"method":"tools/call",
               "params":{"name":name,"arguments":arguments}}),
    )
}

/// The single text block a tool result is made of, insisting there is exactly
/// one — which is the CLI's requirement for the permission tool and this
/// crate's habit everywhere else.
fn only_text(result: &Value) -> String {
    let content = result["content"]
        .as_array()
        .unwrap_or_else(|| panic!("no content array in {result}"));
    assert_eq!(content.len(), 1, "expected one content block: {result}");
    assert_eq!(content[0]["type"], "text");
    content[0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("text is not a string in {result}"))
        .to_owned()
}

/// The permission decision, decoded from the JSON *string* it travels as.
fn decision_of(result: &Value) -> Value {
    serde_json::from_str(&only_text(result)).expect("the text of the block is a JSON string")
}

fn write_args() -> Value {
    json!({
        "tool_name": "Write",
        "input": { "file_path": "/tmp/x", "content": "hello" },
        "tool_use_id": "toolu_01ABC",
    })
}

// ------------------------------------------------------------------- naming --

#[test]
fn qualified_is_the_only_spelling_the_cli_accepts() {
    assert_eq!(
        qualified(REQUEST_PERMISSION),
        "mcp__agent-workspace__request_permission"
    );
    assert_eq!(
        qualified(REPORT_FINDING),
        "mcp__agent-workspace__report_finding"
    );

    // Every name Claude Code 2.1.270 reserves. A config that claims one is
    // dropped with a *warning* on the debug channel and a run that starts
    // anyway with no server at all, so this is asserted rather than trusted.
    for reserved in [
        "workspace",
        "stdio",
        "claude_ai",
        "computer-use",
        "claude-in-chrome",
        "hearthbot",
        "__proto__",
    ] {
        assert_ne!(SERVER_NAME, reserved);
    }
    // The CLI splits the qualified name on `__`, so the server name must not
    // contain one. A hyphen is fine; `agent__workspace` would not be.
    assert!(!SERVER_NAME.contains("__"));

    // The prompt names the tools in full, and nothing checks that for us.
    assert!(workspace_mcp::SYSTEM_PROMPT.contains(&qualified(REPORT_FINDING)));
    assert!(workspace_mcp::SYSTEM_PROMPT.contains(&qualified(REQUEST_PERMISSION)));
}

// ---------------------------------------------------------------- handshake --

#[test]
fn the_handshake_echoes_the_protocol_version_and_lists_both_tools() {
    let stub = Stub::allowing();
    let server = server(&stub);

    let init = ok(
        &server,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-03-26",
            "capabilities":{},
            "clientInfo":{"name":"claude-code","version":"2.1.270"}}}),
    );
    // Echoed, not ours: a version the client did not ask for is a failed
    // negotiation it may disconnect over.
    assert_eq!(init["protocolVersion"], "2025-03-26");
    assert!(init["capabilities"]["tools"].is_object());
    assert_eq!(init["serverInfo"]["name"], SERVER_NAME);
    assert!(init["serverInfo"]["version"].is_string());

    // No requested version at all still produces a usable handshake.
    let bare = ok(
        &server,
        json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{}}),
    );
    assert!(bare["protocolVersion"].as_str().unwrap().starts_with("20"));

    assert_eq!(
        server.handle_line(
            &json!({"jsonrpc":"2.0","method":"notifications/initialized"}).to_string()
        ),
        None
    );

    let tools = ok(
        &server,
        json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}),
    );
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, [REQUEST_PERMISSION, REPORT_FINDING]);

    for tool in tools["tools"].as_array().unwrap() {
        let schema = &tool["inputSchema"];
        assert_eq!(schema["type"], "object", "in {tool}");
        assert!(schema["properties"].is_object(), "in {tool}");
        assert!(schema["required"].is_array(), "in {tool}");
        assert!(
            tool["description"].as_str().is_some_and(|d| !d.is_empty()),
            "the description is what the model reads: {tool}"
        );
    }

    assert_eq!(
        ok(&server, json!({"jsonrpc":"2.0","id":4,"method":"ping"})),
        json!({})
    );
}

// --------------------------------------------------------------- permission --

#[test]
fn an_allow_is_a_json_string_inside_one_text_block() {
    let stub = Stub::allowing();
    let result = tools_call(&server(&stub), REQUEST_PERMISSION, write_args());

    // A denial is a *successful* call; so is an approval. `isError` is about
    // the tool malfunctioning, and this tool did not.
    assert_eq!(result["isError"], json!(false));
    // The nested encoding: the text block's text is a string of JSON, never a
    // JSON object. `only_text` asserts the "exactly one text block" half.
    assert!(result["content"][0]["text"].is_string());

    assert_eq!(
        decision_of(&result),
        json!({ "behavior": "allow", "toolUseID": "toolu_01ABC" })
    );

    let seen = stub.permissions.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].tool_name, "Write");
    assert_eq!(seen[0].input["file_path"], "/tmp/x");
    // Snake case going in, camel with a shouted ID coming out. Both are
    // correct; making them agree breaks one direction.
    assert_eq!(seen[0].tool_use_id.as_deref(), Some("toolu_01ABC"));
}

#[test]
fn a_deny_always_carries_a_message() {
    let stub = Stub::new(Decision::Deny {
        message: "the workspace denied rm -rf".to_owned(),
        interrupt: true,
    });
    let result = tools_call(&server(&stub), REQUEST_PERMISSION, write_args());

    assert_eq!(
        decision_of(&result),
        json!({
            "behavior": "deny",
            // Required by the CLI, and optional in the otherwise-identical
            // hook shape — the trap this assertion exists for.
            "message": "the workspace denied rm -rf",
            "interrupt": true,
            "toolUseID": "toolu_01ABC",
        })
    );
    assert_eq!(result["isError"], json!(false));
}

#[test]
fn an_updated_input_is_sent_only_when_it_would_replace_something() {
    let mut patched = serde_json::Map::new();
    patched.insert("file_path".to_owned(), json!("/tmp/safe"));
    let stub = Stub::new(Decision::Allow {
        updated_input: Some(patched),
    });
    let decision = decision_of(&tools_call(
        &server(&stub),
        REQUEST_PERMISSION,
        write_args(),
    ));
    assert_eq!(decision["updatedInput"], json!({"file_path": "/tmp/safe"}));

    // An empty map is not "replace the input with nothing" — the CLI validates
    // a present `updatedInput` against the target tool's schema, so sending
    // `{}` fails every tool with a required argument. It must be omitted.
    let stub = Stub::new(Decision::Allow {
        updated_input: Some(serde_json::Map::new()),
    });
    let decision = decision_of(&tools_call(
        &server(&stub),
        REQUEST_PERMISSION,
        write_args(),
    ));
    assert_eq!(
        decision,
        json!({"behavior":"allow","toolUseID":"toolu_01ABC"})
    );
}

#[test]
fn a_permission_request_without_a_tool_use_id_omits_the_key_rather_than_nulling_it() {
    let stub = Stub::allowing();
    let result = tools_call(
        &server(&stub),
        REQUEST_PERMISSION,
        json!({"tool_name":"Bash","input":{"command":"ls"}}),
    );
    assert_eq!(decision_of(&result), json!({"behavior":"allow"}));
    assert_eq!(stub.permissions.lock().unwrap()[0].tool_use_id, None);
}

#[test]
fn a_permission_request_missing_its_arguments_never_reaches_the_workspace() {
    let stub = Stub::allowing();
    let server = server(&stub);
    for args in [
        json!({}),
        json!({"input": {}}),
        json!({"tool_name": "Bash"}),
        // `input` has to be an object; a string would reach the UI as a tool
        // input it cannot render.
        json!({"tool_name": "Bash", "input": "ls"}),
    ] {
        let result = tools_call(&server, REQUEST_PERMISSION, args.clone());
        assert_eq!(result["isError"], json!(true), "for {args}");
    }
    assert!(stub.permissions.lock().unwrap().is_empty());
}

// ------------------------------------------------------------------ finding --

#[test]
fn a_finding_reaches_the_workspace_with_its_payload_intact() {
    let stub = Stub::allowing();
    let payload = json!({
        "severity": "high",
        "title": "refresh tokens are logged in plaintext",
        "body": "src/auth.ts:42 writes the token to the request log.",
        "references": ["src/auth.ts:42"],
        // §7's degradation invariant: an unknown field is kept, not dropped.
        "confidence": 0.8,
    });
    let result = tools_call(&server(&stub), REPORT_FINDING, payload.clone());

    assert_eq!(result["isError"], json!(false));
    assert!(
        !only_text(&result).is_empty(),
        "the ack says it was recorded"
    );

    let reports = stub.reports.lock().unwrap();
    // The BARE name, not the mcp__agent-workspace__ spelling.
    assert_eq!(reports[0].0, REPORT_FINDING);
    assert_eq!(reports[0].1, payload);
}

#[test]
fn a_finding_outside_the_severity_ladder_is_refused() {
    let stub = Stub::allowing();
    let server = server(&stub);
    // The message is transcript the model reads and corrects itself from, so
    // each one has to name what was wrong — the ladder, or the missing field.
    for (args, needle) in [
        (json!({"severity":"urgent","title":"x"}), "critical"),
        (json!({"severity":"High","title":"x"}), "critical"),
        (json!({"title":"x"}), "critical"),
        (json!({"severity":"high"}), "title"),
    ] {
        let result = tools_call(&server, REPORT_FINDING, args.clone());
        assert_eq!(result["isError"], json!(true), "for {args}");
        assert!(only_text(&result).contains(needle), "for {args}");
    }
    assert!(stub.reports.lock().unwrap().is_empty());
}

// ----------------------------------------------------------------- protocol --

#[test]
fn malformed_input_becomes_a_jsonrpc_error_and_never_a_panic() {
    let stub = Stub::allowing();
    let server = server(&stub);

    for line in [
        "{",
        "not json at all",
        "[]",
        "[{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}]",
        "\"a bare string\"",
        "17",
        "null",
        "{\"jsonrpc\":\"2.0\",\"id\":1}",
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":42}",
        "{\"jsonrpc\":\"1.0\",\"id\":1,\"method\":\"ping\"}",
    ] {
        let out = server
            .handle_line(line)
            .unwrap_or_else(|| panic!("no answer to {line}"));
        let response: Value =
            serde_json::from_str(&out).unwrap_or_else(|e| panic!("{out} is not JSON: {e}"));
        assert_eq!(response["jsonrpc"], "2.0", "for {line}");
        assert!(response["error"]["code"].is_i64(), "for {line}");
        assert!(
            response["error"]["message"]
                .as_str()
                .is_some_and(|m| !m.is_empty()),
            "for {line}"
        );
        // The `id` key is always present, null when the line was too broken to
        // recover one.
        assert!(response.get("id").is_some(), "for {line}");
    }

    // Framing noise is not a request and gets no answer: a parse error here
    // would put a line on the wire that nothing asked for.
    assert_eq!(server.handle_line(""), None);
    assert_eq!(server.handle_line("   \t "), None);
}

#[test]
fn an_unknown_method_is_method_not_found() {
    let stub = Stub::allowing();
    let response = call(
        &server(&stub),
        json!({"jsonrpc":"2.0","id":9,"method":"resources/list"}),
    );
    assert_eq!(response["error"]["code"], -32601);
}

#[test]
fn an_unknown_tool_is_an_is_error_result_rather_than_a_protocol_error() {
    let stub = Stub::allowing();
    let response = call(
        &server(&stub),
        json!({"jsonrpc":"2.0","id":10,"method":"tools/call",
               "params":{"name":"open_plan","arguments":{}}}),
    );
    // MCP's own distinction: a tool the server does not have is something the
    // model should see and correct, so it is a result. A JSON-RPC error would
    // be a transport failure the model never sees.
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(response["result"]["isError"], json!(true));
    assert!(only_text(&response["result"]).contains("open_plan"));

    // A `tools/call` with no tool named at all is a different thing: the
    // caller's request is malformed, not the model's choice of tool.
    let response = call(
        &server(&stub),
        json!({"jsonrpc":"2.0","id":11,"method":"tools/call","params":{}}),
    );
    assert_eq!(response["error"]["code"], -32602);
}

#[test]
fn a_notification_is_never_answered() {
    let stub = Stub::allowing();
    let server = server(&stub);
    for line in [
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}),
        // Unknown, and still not answered: -32601 to a notification is a
        // protocol violation, not helpfulness.
        json!({"jsonrpc":"2.0","method":"notifications/whatever"}),
        // A null id means the same as no id.
        json!({"jsonrpc":"2.0","id":null,"method":"ping"}),
    ] {
        assert_eq!(server.handle_line(&line.to_string()), None, "for {line}");
    }
}

// ------------------------------------------------------------------- socket --

#[test]
fn the_mcp_config_is_the_shape_the_cli_parses() {
    let dir = TempDir::new("mcp-config");
    let listener = Listener::bind(dir.path(), Stub::allowing()).unwrap();
    let relay = Path::new("/opt/agent-workspace/bin/workspace-mcp-relay");

    let raw = listener.mcp_config(relay);
    assert!(
        raw.trim_start().starts_with('{'),
        "--mcp-config decides between a path and inline JSON by the leading brace: {raw}"
    );
    assert!(!raw.contains('\n'), "it goes on an argv: {raw}");

    let config: Value = serde_json::from_str(&raw).unwrap();
    // Not "servers", not "mcp_servers". The CLI names this key in its error
    // message, which is how we know it is the one it reads.
    let server = &config["mcpServers"][SERVER_NAME];
    assert!(server.is_object(), "in {raw}");
    assert_eq!(server["type"], "stdio");
    assert_eq!(server["command"], relay.display().to_string());
    assert!(
        server["command"].as_str().is_some_and(|c| !c.is_empty()),
        "command must be a non-empty string"
    );
    assert_eq!(server["args"], json!([]));
    assert_eq!(
        server["env"][SOCKET_ENV],
        json!(listener.socket_path().display().to_string())
    );
    assert_eq!(listener.socket_path(), dir.path().join("mcp.sock"));
}

#[test]
fn a_real_unix_socket_round_trips_a_permission_request() {
    let dir = TempDir::new("mcp-socket");
    let stub = Stub::new(Decision::Deny {
        message: "not in this workspace".to_owned(),
        interrupt: false,
    });
    let listener = Listener::bind(
        &dir.path().join("run-1842"),
        Arc::clone(&stub) as Arc<dyn Workspace>,
    )
    .unwrap();

    // The socket lives outside the run's worktree (§15) and the directory it
    // lives in is not readable by anyone else.
    let mode = std::fs::metadata(listener.socket_path().parent().unwrap())
        .unwrap()
        .permissions();
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
        0o700
    );

    // Two connections, because nothing in the protocol promises one relay.
    let mut first = Conn::open(listener.socket_path());
    let mut second = Conn::open(listener.socket_path());

    let init = first.call(json!({"jsonrpc":"2.0","id":1,"method":"initialize",
                                 "params":{"protocolVersion":"2025-06-18"}}));
    assert_eq!(init["result"]["serverInfo"]["name"], SERVER_NAME);
    // A notification over a real socket must not produce a line; if it did,
    // the next `read_line` would return it and every reply after it would be
    // off by one.
    first.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));

    let response = second.call(json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
        "params":{"name":REQUEST_PERMISSION,"arguments":write_args()}}));
    assert_eq!(decision_of(&response["result"])["behavior"], "deny");

    let response = first.call(json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
        "params":{"name":REPORT_FINDING,
                  "arguments":{"severity":"low","title":"a note from the agent"}}}));
    assert_eq!(response["result"]["isError"], json!(false));

    assert_eq!(stub.permissions.lock().unwrap().len(), 1);
    assert_eq!(stub.reports.lock().unwrap().len(), 1);

    // Binding twice while the first is live is refused rather than silently
    // stealing the path.
    let second_bind = Listener::bind(&dir.path().join("run-1842"), Stub::allowing());
    assert!(second_bind.is_err(), "a live socket must not be replaced");

    let path = listener.socket_path().to_path_buf();
    listener.shutdown();
    assert!(!path.exists(), "shutdown unlinks the socket");

    // A stale socket file left by a crash is reclaimed rather than reported —
    // otherwise one crash would poison the directory forever.
    std::fs::write(&path, b"").unwrap();
    let again = Listener::bind(&dir.path().join("run-1842"), Stub::allowing()).unwrap();
    assert_eq!(again.socket_path(), path);
}

#[test]
fn a_dropped_listener_leaves_no_socket_behind() {
    let dir = TempDir::new("mcp-drop");
    let path = {
        let listener = Listener::bind(dir.path(), Stub::allowing()).unwrap();
        listener.socket_path().to_path_buf()
    };
    assert!(!path.exists(), "drop unlinks the socket");
}

#[test]
fn the_relay_carries_a_session_from_a_child_process() {
    // The one thing that cannot be asserted without spawning something, and
    // therefore the only test here that does. It stands in for Claude Code:
    // spawn the relay with nothing but the env map the mcp-config supplies,
    // talk MCP over its stdio, and expect the workspace on the other end.
    let dir = TempDir::new("mcp-relay");
    let stub = Stub::allowing();
    let listener = Listener::bind(dir.path(), Arc::clone(&stub) as Arc<dyn Workspace>).unwrap();

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_workspace-mcp-relay"))
        // Deliberately the only variable it gets: `driver::process::spawn`
        // hands the `claude` child no environment, so this map is the whole
        // channel.
        .env(SOCKET_ENV, listener.socket_path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("the relay binary is built alongside this test");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
               "params":{"name":REQUEST_PERMISSION,"arguments":write_args()}})
    )
    .unwrap();
    stdin.flush().unwrap();

    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let response: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(decision_of(&response["result"])["behavior"], "allow");
    assert_eq!(stub.permissions.lock().unwrap()[0].tool_name, "Write");

    // Closing stdin is how an agent ends the session. The relay half-closes
    // the socket, the workspace's reader sees EOF and closes, and the relay
    // exits rather than sitting on a socket for the life of the machine.
    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "the relay exited with {status}");
}

#[test]
fn the_relay_refuses_to_run_without_a_socket_to_dial() {
    // Someone will run this binary by hand. It has to say why nothing happens,
    // and it has to say it on stderr: stdout belongs to the protocol.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_workspace-mcp-relay"))
        .env_remove(SOCKET_ENV)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "stdout belongs to the protocol");
    assert!(String::from_utf8_lossy(&out.stderr).contains(SOCKET_ENV));
}

/// A client of the socket, framed the way the relay's bytes arrive.
struct Conn {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Conn {
    fn open(path: &Path) -> Conn {
        let stream = UnixStream::connect(path).unwrap();
        Conn {
            reader: BufReader::new(stream.try_clone().unwrap()),
            writer: stream,
        }
    }

    fn send(&mut self, request: Value) {
        writeln!(self.writer, "{request}").unwrap();
        self.writer.flush().unwrap();
    }

    fn call(&mut self, request: Value) -> Value {
        self.send(request);
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{line:?} is not JSON: {e}"))
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!(
            "workspace-mcp-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
