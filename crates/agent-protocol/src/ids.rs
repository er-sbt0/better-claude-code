use serde::{Deserialize, Serialize};
use ts_rs::TS;

macro_rules! string_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
        #[ts(export)]
        pub struct $name(pub String);

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

string_id!(
    /// Our identifier for a run. Ours, not the provider's — §14 keeps those
    /// separate on purpose.
    RunId
);

string_id!(
    /// The provider's session identifier. For Claude Code we generate it and
    /// pass `--session-id`, rather than reading it back out of the stream (§14).
    SessionId
);

string_id!(
    /// Correlates `tool.started` with the `tool.completed` that answers it.
    ToolUseId
);
