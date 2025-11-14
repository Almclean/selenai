use anyhow::{Result, anyhow};
use async_trait::async_trait;
use tokio::sync::mpsc::UnboundedSender;

use crate::types::{Message, Role, ToolInvocation};

pub mod openai;

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub messages: Vec<Message>,
    pub stream: bool,
    pub system_prompt: Option<String>,
    pub tools: Vec<LlmTool>,
}

impl ChatRequest {
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            messages,
            stream: false,
            system_prompt: None,
            tools: Vec::new(),
        }
    }

    pub fn with_stream(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    pub fn with_tool(mut self, tool: LlmTool) -> Self {
        self.tools.push(tool);
        self
    }

    pub fn latest_user_prompt(&self) -> Option<&str> {
        self.messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, Role::User))
            .map(|m| m.content.as_str())
    }
}

#[derive(Debug, Clone)]
pub enum ChatResponse {
    Assistant(Message),
    ToolCall(ToolInvocation),
}

impl ChatResponse {
    pub fn assistant_text(text: impl Into<String>) -> Self {
        ChatResponse::Assistant(Message::new(Role::Assistant, text))
    }
}

#[derive(Debug, Clone)]
pub enum StreamEvent {
    Delta(String),
    ToolCall(ToolInvocation),
    Completed,
}

pub type StreamEventSender = UnboundedSender<StreamEvent>;

#[derive(Debug, Clone)]
pub struct LlmTool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl LlmTool {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }

    pub fn to_openai_json(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            }
        })
    }
}

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse>;

    async fn chat_stream(&self, request: ChatRequest, sender: StreamEventSender) -> Result<()>;

    fn supports_streaming(&self) -> bool {
        true
    }
}

pub struct StubClient;

impl StubClient {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl LlmClient for StubClient {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let turn = request
            .messages
            .iter()
            .filter(|m| matches!(m.role, Role::User))
            .count();

        let Some(prompt) = request.latest_user_prompt() else {
            return Err(anyhow!("stub client requires at least one user prompt"));
        };

        let trimmed = prompt.trim();
        if trimmed.is_empty() {
            return Ok(ChatResponse::assistant_text(
                "I need some text to work with.",
            ));
        }

        if trimmed.contains("lua") {
            return Ok(ChatResponse::assistant_text(
                "Try `/lua rust.read_file(\"Cargo.toml\")` to inspect a file.",
            ));
        }

        Ok(ChatResponse::assistant_text(format!(
            "Stub agent turn {} heard: \"{}\"",
            turn, trimmed
        )))
    }

    async fn chat_stream(&self, request: ChatRequest, sender: StreamEventSender) -> Result<()> {
        let response = self.chat(request).await?;
        match response {
            ChatResponse::Assistant(message) => {
                if !message.content.is_empty() {
                    let _ = sender.send(StreamEvent::Delta(message.content));
                }
            }
            ChatResponse::ToolCall(call) => {
                let _ = sender.send(StreamEvent::ToolCall(call));
            }
        }
        let _ = sender.send(StreamEvent::Completed);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_request_builders_attach_prompt_and_tool() {
        let messages = vec![Message::new(Role::User, "hello")];
        let tool = LlmTool::new(
            "lua_run_script",
            "Run Lua",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "source": { "type": "string" }
                },
                "required": ["source"]
            }),
        );

        let request = ChatRequest::new(messages.clone())
            .with_stream(true)
            .with_system_prompt("system guidance")
            .with_tool(tool.clone());

        assert!(request.stream);
        assert_eq!(request.messages.len(), 1);
        assert_eq!(request.system_prompt.as_deref(), Some("system guidance"));
        assert_eq!(request.tools.len(), 1);
        assert_eq!(request.tools[0].name, tool.name);
    }
}
