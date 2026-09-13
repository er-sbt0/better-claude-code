//! The MCP surface: the handshake, the tool list, and the two tools.
//!
//! Everything here is a pure function of one line of JSON plus whatever the
//! [`Workspace`] decides, which is deliberate: it is what lets the wire shapes
//! in `tests/mcp.rs` be asserted without a socket, a process, or a running
//! agent. The socket in `listener` is a transport around this and adds no
//! protocol of its own.

use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::jsonrpc::{self, Incoming, INVALID_PARAMS, METHOD_NOT_FOUND};
use crate::{qualified, REPORT_FINDING, REQUEST_PERMISSION, SERVER_NAME};

/// The revision we answer with when the client asked for nothing usable.
///
/// The latest at the time this was written. It is only a fallback: see
/// [`Server::initialize`] for why the client's own value wins.
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";

/// §6's severity ladder, exactly. The agent picks one of these or the call is
/// refused — an open string would put five spellings of "high" in the log
/// within a week, and the review surface has to sort by this.
const SEVERITIES: [&str; 5] = ["info", "low", "medium", "high", "critical"];

/// A permission decision Claude Code's own permission layer is waiting on.
///
/// Note what this is *not*: a `tool_use` block on the wire. The CLI's
/// permission layer makes this call, not the model, so no amount of reading the
/// event stream produces it. That is precisely what lets §15 require that
/// permission requests arrive on the runtime's own channel and mean something
/// by it — this server observing the call *is* that channel.
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRequest {
    /// The tool the agent wants to run: `Bash`, `Write`, `Edit`, …
    pub tool_name: String,
    /// Its arguments, in that tool's own schema. Untrusted (§15): it is the
    /// model's text, and it is what the UI will render.
    pub input: Value,
    /// `toolu_…`. Optional because the schema says so, though the CLI has
    /// always sent it.
    pub tool_use_id: Option<String>,
}

/// The answer, in the two shapes the CLI accepts.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Allow {
        /// Replaces the tool's input outright when present and non-empty — it
        /// is not a patch, and it is validated against the target tool's own
        /// schema, so a mismatch fails the call with
        /// `permission_updated_input_invalid` rather than falling back to what
        /// the model sent. `None` (and an empty map, which is normalized to
        /// the same thing) accepts the input unchanged.
        updated_input: Option<Map<String, Value>>,
    },
    Deny {
        /// **Required by the CLI**, and the only thing the model is told. It is
        /// optional in the otherwise-identical `PermissionRequest` *hook*
        /// shape, which is a good way to be misled into omitting it.
        message: String,
        /// Abort the turn instead of returning an error to the model.
        interrupt: bool,
    },
}

/// What the workspace does when the agent reports something.
///
/// Implemented by the driver, which is where the event log, the run and the UI
/// are. §15 constrains every implementation: nothing behind these methods may
/// mutate policy, spawn a run, or write outside the run's worktree. These tools
/// are reporting, not control, and [`Self::request_permission`] is the
/// workspace *deciding* — never the agent granting itself anything.
///
/// `Send + Sync` because one connection is served per thread.
pub trait Workspace: Send + Sync {
    fn request_permission(&self, req: PermissionRequest) -> Decision;

    /// A report from the agent. `tool` is the BARE name ([`REPORT_FINDING`]),
    /// not the `mcp__agent-workspace__…` spelling, so a match arm here reads
    /// like the tool list rather than like an argv.
    ///
    /// `payload` is the arguments object as it arrived, validated only against
    /// the tool's declared schema and otherwise unreshaped: §7's degradation
    /// invariant says an unknown field is kept, not dropped, and this is the
    /// first place that rule has teeth.
    fn report(&self, tool: &str, payload: Value);
}

/// The protocol, with no transport under it.
pub struct Server {
    workspace: Arc<dyn Workspace>,
}

impl Server {
    pub fn new(workspace: Arc<dyn Workspace>) -> Self {
        Server { workspace }
    }

    /// One JSON-RPC line in, at most one line out; `None` for a notification.
    ///
    /// Never panics and never emits a malformed line. Both halves are load
    /// bearing: this runs on a socket thread the driver does not supervise, so
    /// a panic here is a silently dead MCP server and every subsequent
    /// permission request in the run fails, and a malformed line desynchronizes
    /// a stream that has no resynchronization mechanism.
    ///
    /// The returned line carries no trailing newline — framing belongs to the
    /// transport.
    pub fn handle_line(&self, line: &str) -> Option<String> {
        match jsonrpc::parse(line) {
            Incoming::Blank => None,
            // A notification is never answered, not even to say we do not know
            // the method.
            Incoming::Notify => None,
            Incoming::Malformed { id, code, message } => {
                Some(jsonrpc::error(id.as_ref(), code, &message))
            }
            Incoming::Call { id, method, params } => Some(match self.call(&method, &params) {
                Ok(result) => jsonrpc::result(&id, result),
                Err((code, message)) => jsonrpc::error(Some(&id), code, &message),
            }),
        }
    }

    fn call(&self, method: &str, params: &Value) -> Result<Value, (i64, String)> {
        match method {
            "initialize" => Ok(self.initialize(params)),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => self.tools_call(params),
            // A keepalive. Its result is required to be an empty object.
            "ping" => Ok(json!({})),
            other => Err((
                METHOD_NOT_FOUND,
                format!("unknown method {other:?}; this server implements initialize, tools/list, tools/call and ping"),
            )),
        }
    }

    fn initialize(&self, params: &Value) -> Value {
        // Echo the client's requested revision rather than announcing our own.
        //
        // The handshake is a negotiation: a server that answers with a
        // different version is telling the client it could not agree, and a
        // client is entitled to disconnect over that. We implement only the
        // parts of MCP that have been identical across every revision that
        // exists — initialize, tools/list, tools/call, ping — so there is no
        // version we would have to refuse, and echoing means this crate does
        // not need a release to keep working when the CLI moves forward.
        let version = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_PROTOCOL_VERSION);
        json!({
            "protocolVersion": version,
            // Only tools. No resources, no prompts, no sampling: §15's
            // reporting-not-control rule is easiest to keep true when there is
            // nothing else on the server to reason about.
            "capabilities": { "tools": {} },
            "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
        })
    }

    fn tools_call(&self, params: &Value) -> Result<Value, (i64, String)> {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return Err((
                INVALID_PARAMS,
                "tools/call requires a string `name`".to_owned(),
            ));
        };
        let empty = json!({});
        let args = params.get("arguments").unwrap_or(&empty);

        Ok(match name {
            REQUEST_PERMISSION => self.request_permission(args),
            REPORT_FINDING => self.report_finding(args),
            // An unknown tool is a *result* with `isError: true`, not a
            // JSON-RPC error. That is the MCP convention and it is not
            // arbitrary: the model is supposed to see this and correct itself,
            // and a JSON-RPC error is a transport-level failure the model never
            // sees.
            other => tool_error(format!(
                "unknown tool {other:?}; this server exposes {} and {}",
                qualified(REQUEST_PERMISSION),
                qualified(REPORT_FINDING),
            )),
        })
    }

    fn request_permission(&self, args: &Value) -> Value {
        let Some(tool_name) = args.get("tool_name").and_then(Value::as_str) else {
            return tool_error("request_permission requires a string `tool_name`".to_owned());
        };
        let Some(input) = args.get("input").filter(|v| v.is_object()) else {
            return tool_error("request_permission requires an object `input`".to_owned());
        };
        let tool_use_id = args
            .get("tool_use_id")
            .and_then(Value::as_str)
            .map(str::to_owned);

        let decision = self.workspace.request_permission(PermissionRequest {
            tool_name: tool_name.to_owned(),
            input: input.clone(),
            tool_use_id: tool_use_id.clone(),
        });

        // The nested encoding the CLI demands: exactly one content block, of
        // type "text", whose text is a JSON *string* rather than a JSON object.
        // Anything else — two blocks, a non-text block, an object where the
        // string goes — is rejected with "Permission prompt tool returned an
        // invalid result", and the run's next permission request fails the same
        // way. This is the single most breakable line in the crate.
        text_result(
            serde_json::to_string(&decision_payload(&decision, tool_use_id.as_deref()))
                .expect("a serde_json::Value always serializes"),
        )
    }

    fn report_finding(&self, args: &Value) -> Value {
        let Some(severity) = args.get("severity").and_then(Value::as_str) else {
            return tool_error(format!(
                "report_finding requires a string `severity`, one of {}",
                SEVERITIES.join(", ")
            ));
        };
        if !SEVERITIES.contains(&severity) {
            return tool_error(format!(
                "unknown severity {severity:?}; use one of {}",
                SEVERITIES.join(", ")
            ));
        }
        let Some(title) = args.get("title").and_then(Value::as_str) else {
            return tool_error("report_finding requires a string `title`".to_owned());
        };

        self.workspace.report(REPORT_FINDING, args.clone());

        // Short and free of instructions. Whatever comes back here is
        // transcript the model will react to, so it says that the record was
        // made and nothing that invites another call.
        text_result(format!("Recorded {severity} finding: {title}"))
    }
}

/// The decision, in the CLI's own spelling.
///
/// **The casing flips between request and response.** It is `tool_use_id`
/// arriving and `toolUseID` going back — not `toolUseId`, and not the snake
/// case of the request. Both spellings are correct in their own direction;
/// "fixing" the inconsistency breaks one of them.
fn decision_payload(decision: &Decision, tool_use_id: Option<&str>) -> Value {
    let mut out = Map::new();
    match decision {
        Decision::Allow { updated_input } => {
            out.insert("behavior".to_owned(), json!("allow"));
            // Omitted when empty on purpose: present-and-empty is not the same
            // as absent. A present `updatedInput` *replaces* the input and is
            // schema-validated, so sending `{}` would fail every tool with a
            // required argument.
            if let Some(input) = updated_input.as_ref().filter(|m| !m.is_empty()) {
                out.insert("updatedInput".to_owned(), Value::Object(input.clone()));
            }
        }
        Decision::Deny { message, interrupt } => {
            out.insert("behavior".to_owned(), json!("deny"));
            out.insert("message".to_owned(), json!(message));
            out.insert("interrupt".to_owned(), json!(interrupt));
        }
    }
    if let Some(id) = tool_use_id {
        out.insert("toolUseID".to_owned(), json!(id));
    }
    // `decisionClassification` is deliberately absent. Unset, the CLI infers
    // conservatively — temporary for allow, reject for deny — which is exactly
    // what a one-off decision means. §12's "Allow for workspace" button is what
    // will need `user_permanent`, and it needs a `Decision` variant to carry
    // it rather than a guess made here.
    Value::Object(out)
}

fn text_result(text: String) -> Value {
    // `isError: false` is explicit because a *denied* permission is a
    // successful tool call. Marking a deny as an error would tell the model the
    // permission system malfunctioned rather than that the answer was no.
    json!({ "content": [{ "type": "text", "text": text }], "isError": false })
}

fn tool_error(text: String) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": true })
}

/// The tool list, with schemas the agent actually reads.
///
/// The `description` fields are not documentation — they are the other half of
/// [`crate::SYSTEM_PROMPT`], and they are what the model consults when deciding
/// whether this call is warranted.
fn tools() -> Value {
    json!([
        {
            "name": REQUEST_PERMISSION,
            "description":
                "Called by the harness when a tool needs approval. Not for the model to call: \
                 it grants nothing, and its answer is the workspace's decision.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tool_name": { "type": "string", "description": "The tool awaiting approval." },
                    "input": { "type": "object", "description": "That tool's arguments." },
                    "tool_use_id": { "type": "string", "description": "The tool_use id, when the caller has one." },
                },
                "required": ["tool_name", "input"],
                // No `additionalProperties: false`. The caller of this tool is
                // the CLI's permission layer, not the model, and a field it
                // adds in a future version must not turn every permission
                // request into a validation failure.
            },
        },
        {
            "name": REPORT_FINDING,
            "description":
                "Record something a reviewer of this run should see: a bug, a risk, a broken \
                 assumption, a security or data-loss concern, a decision that could have gone \
                 the other way. Call it as soon as you know the thing rather than saving it for \
                 a summary, and say the same thing in your reply — this is a record, not a \
                 substitute for telling the user. It changes nothing about the run.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "severity": {
                        "type": "string",
                        "enum": SEVERITIES,
                        "description": "How much this matters to someone reviewing the run.",
                    },
                    "title": {
                        "type": "string",
                        "description": "One line, specific enough to be useful in a list.",
                    },
                    "body": {
                        "type": "string",
                        "description": "What it is, why it matters, and what you did about it.",
                    },
                    "references": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Repository-relative paths, optionally path:line.",
                    },
                },
                "required": ["severity", "title"],
            },
        },
    ])
}
