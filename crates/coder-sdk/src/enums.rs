//! Open enums for the values scuttle matches on. Unknown values from a newer server are preserved.

macro_rules! open_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant,)+
            /// A value this SDK version does not know yet.
            Unknown(String),
        }

        impl $name {
            pub fn parse(value: &str) -> Self {
                match value {
                    $($text => $name::$variant,)+
                    other => $name::Unknown(other.to_owned()),
                }
            }

            pub fn as_str(&self) -> &str {
                match self {
                    $($name::$variant => $text,)+
                    $name::Unknown(other) => other,
                }
            }
        }
    };
}

open_enum!(
    /// `codersdk.ChatStatus`.
    ChatStatus {
        Waiting => "waiting",
        Running => "running",
        Interrupting => "interrupting",
        RequiresAction => "requires_action",
        Error => "error",
    }
);

open_enum!(
    /// `codersdk.ChatStreamEventType`.
    StreamEventType {
        MessagePart => "message_part",
        Message => "message",
        Status => "status",
        Error => "error",
        QueueUpdate => "queue_update",
        Retry => "retry",
        ActionRequired => "action_required",
        PreviewReset => "preview_reset",
        HistoryReset => "history_reset",
    }
);

open_enum!(
    /// `codersdk.ChatMessagePartType`.
    PartType {
        Text => "text",
        Reasoning => "reasoning",
        ToolCall => "tool-call",
        ToolResult => "tool-result",
        Source => "source",
        File => "file",
        FileReference => "file-reference",
        ContextFile => "context-file",
        Skill => "skill",
        WorkspaceFileReference => "workspace-file-reference",
        HookContext => "hook-context",
        HookNotice => "hook-notice",
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_known_and_unknown_values() {
        assert_eq!(ChatStatus::parse("running"), ChatStatus::Running);
        assert_eq!(ChatStatus::parse("paused").as_str(), "paused");
        assert_eq!(PartType::parse("tool-call"), PartType::ToolCall);
    }
}
