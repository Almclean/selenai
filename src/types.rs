use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn display_name(&self) -> &'static str {
        match self {
            Role::User => "You",
            Role::Assistant => "Assistant",
            Role::Tool => "Tool",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    pub tool_call_id: Option<String>,
    pub tool_calls: Vec<ToolInvocation>,
}

impl Message {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_call_id: None,
            tool_calls: Vec::new(),
        }
    }

    pub fn new_tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_call_id: Some(tool_call_id.into()),
            tool_calls: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInvocation {
    pub name: String,
    pub arguments: JsonValue,
    pub call_id: Option<String>,
}

impl ToolInvocation {
    pub fn from_parts(
        name: impl Into<String>,
        arguments: JsonValue,
        call_id: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            arguments,
            call_id,
        }
    }

    pub fn to_openai_tool_call(&self) -> JsonValue {
        let args_string = serde_json::to_string(&self.arguments).unwrap_or_else(|_| "null".into());
        let mut value = serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name,
                "arguments": args_string,
            }
        });
        if let Some(id) = &self.call_id {
            value["id"] = JsonValue::String(id.clone());
        }
        value
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolStatus {
    Pending,
    Success,
    Error,
}

impl ToolStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolStatus::Pending => "pending",
            ToolStatus::Success => "ok",
            ToolStatus::Error => "error",
        }
    }
}

impl fmt::Display for ToolStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolLogEntry {
    pub id: usize,
    pub title: String,
    pub status: ToolStatus,
    pub detail: String,
}

impl ToolLogEntry {
    pub fn new(id: usize, title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            status: ToolStatus::Pending,
            detail: detail.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_display_names_are_human_readable() {
        assert_eq!(Role::User.display_name(), "You");
        assert_eq!(Role::Assistant.display_name(), "Assistant");
        assert_eq!(Role::Tool.display_name(), "Tool");
    }

    #[test]
    fn tool_status_strings_match_display() {
        for (status, label) in [
            (ToolStatus::Pending, "pending"),
            (ToolStatus::Success, "ok"),
            (ToolStatus::Error, "error"),
        ] {
            assert_eq!(status.as_str(), label);
            assert_eq!(status.to_string(), label);
        }
    }

    #[test]
    fn tool_invocation_serializes_to_openai_shape() {
        let invocation = ToolInvocation::from_parts(
            "lua_run_script",
            serde_json::json!({"source": "return 1"}),
            Some("call_123".into()),
        );
        let json = invocation.to_openai_tool_call();
        assert_eq!(json["function"]["name"], "lua_run_script");
        assert_eq!(json["id"], "call_123");
        assert_eq!(
            json["function"]["arguments"],
            serde_json::json!(r#"{"source":"return 1"}"#)
        );
    }
}
