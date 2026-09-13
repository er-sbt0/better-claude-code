//! Wire types for the append-only run event log.
//!
//! This crate is the single source of truth for these shapes. The TypeScript in
//! `packages/shared-types/` is generated from it and must never be hand-edited
//! (design §22).
//!
//! The distinction that shapes everything here: a **raw event** is something we
//! observed and can never recompute, so it is stored forever; a **projection**
//! is something we concluded, so it is disposable. Only raw events live in this
//! crate (design §8).

pub mod event;
pub mod ids;
pub mod usage;

pub use event::{
    AssistantMessage, AssistantThinking, Event, PermissionRequested, PermissionResolved,
    ProcessOutput, RawEvent, RunFailed, RunFinished, RunRateLimited, RunStarted, ToolCompleted,
    ToolResult, ToolStarted, TurnFinished, TurnStarted, Unclassified, UnclassifiedReason,
    UserMessage, WorkspaceReport, SCHEMA_VERSION, WORKSPACE_MCP_SERVER, WORKSPACE_TOOL_PREFIX,
};
pub use ids::{RunId, SessionId, ToolUseId};
pub use usage::{ModelUsage, Usage};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The two halves of the server's identity have to agree, and one of them
    /// is a literal because `const` cannot be interpolated.
    ///
    /// Worth a test rather than a comment: getting this wrong does not fail
    /// loudly. Claude Code drops an unknown server with a debug-channel
    /// warning, so the symptom of a drifted prefix is reports quietly never
    /// being recognized — which reads as an agent that chose not to report.
    #[test]
    fn the_tool_prefix_matches_the_server_name() {
        assert_eq!(
            WORKSPACE_TOOL_PREFIX,
            format!("mcp__{WORKSPACE_MCP_SERVER}__")
        );
        assert_ne!(
            WORKSPACE_MCP_SERVER, "workspace",
            "`workspace` is reserved by Claude Code and is silently not loaded"
        );
    }

    /// The envelope must serialize exactly as design §8 draws it: `type` and
    /// `payload` at the top level, beside the envelope fields.
    #[test]
    fn envelope_matches_the_documented_shape() {
        let ev = RawEvent {
            run_id: RunId::from("run_1842"),
            seq: 91,
            ts: "2026-09-12T14:22:08.412Z".to_owned(),
            schema: SCHEMA_VERSION,
            idempotency_key: Some("uuid-abc".to_owned()),
            event: Event::TurnStarted(TurnStarted { index: 0 }),
        };

        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["run_id"], json!("run_1842"));
        assert_eq!(v["seq"], json!(91));
        assert_eq!(v["type"], json!("turn.started"));
        assert_eq!(v["payload"], json!({ "index": 0 }));

        let back: RawEvent = serde_json::from_value(v).unwrap();
        assert_eq!(back, ev);
    }

    /// §7: block payloads must round-trip through JSON without loss, because
    /// the event log is forever.
    #[test]
    fn unclassified_preserves_the_line_verbatim() {
        let line = "{\"type\":\"something_new\",\"x\":[1,2,3]}";
        let ev = Event::Unclassified(Unclassified {
            source_type: Some("something_new".to_owned()),
            source_subtype: None,
            reason: UnclassifiedReason::UnknownType,
            line: line.to_owned(),
        });
        let round: Event = serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
        assert_eq!(round, ev);
    }

    #[test]
    fn tool_result_models_both_provider_shapes() {
        let structured: ToolResult =
            serde_json::from_str(r#"{"stdout":"hi","stderr":"","interrupted":false}"#).unwrap();
        assert!(matches!(structured, ToolResult::Structured(_)));

        let text: ToolResult = serde_json::from_str(r#""Error: Exit code 1""#).unwrap();
        assert!(matches!(text, ToolResult::Text(_)));
    }

    #[test]
    fn usage_totals_are_summed_over_models() {
        let usage = Usage::from_models(
            0.0226564,
            vec![
                ModelUsage {
                    model: "claude-sonnet-5".into(),
                    input_tokens: 100,
                    output_tokens: 10,
                    cost_usd: 0.0217034,
                    ..Default::default()
                },
                ModelUsage {
                    model: "claude-haiku-4-5-20251001".into(),
                    input_tokens: 20,
                    output_tokens: 5,
                    cost_usd: 0.000953,
                    ..Default::default()
                },
            ],
        );
        assert_eq!(usage.input_tokens, 120);
        assert_eq!(usage.output_tokens, 15);
        assert!(usage.cost_reconciles());
    }
}
