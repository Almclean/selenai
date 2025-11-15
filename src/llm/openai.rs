use std::{collections::HashMap, env};

use anyhow::{Context, Result, anyhow};
use futures_util::StreamExt;
use reqwest::{
    Client,
    header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue},
};
use serde_json::{Value, json};

use crate::types::{Message, Role, ToolInvocation};

use super::{ChatRequest, ChatResponse, LlmClient, LlmTool, StreamEvent, StreamEventSender};

const ORG_HEADER: &str = "openai-organization";
const PROJECT_HEADER: &str = "openai-project";

#[derive(Clone, Debug)]
pub struct OpenAiConfig {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
    pub organization: Option<String>,
    pub project: Option<String>,
}

pub struct OpenAiClient {
    http: Client,
    config: OpenAiConfig,
}

impl OpenAiClient {
    pub fn new(config: OpenAiConfig) -> Result<Self> {
        let http = Client::builder()
            .default_headers(build_default_headers(&config)?)
            .build()?;

        Ok(Self { http, config })
    }

    fn build_payload(&self, request: &ChatRequest, stream: bool) -> Value {
        let mut messages = Vec::new();

        if let Some(prompt) = &request.system_prompt {
            messages.push(json!({
                "role": "system",
                "content": prompt,
            }));
        }

        for message in &request.messages {
            if let Some(serialized) = serialize_message(message) {
                messages.push(serialized);
            }
        }

        let mut payload = json!({
            "model": self.config.model,
            "stream": stream,
            "messages": messages,
        });

        if !request.tools.is_empty() {
            let tools = request
                .tools
                .iter()
                .map(LlmTool::to_openai_json)
                .collect::<Vec<_>>();
            payload["tools"] = Value::Array(tools);
        }

        payload
    }
}

fn build_default_headers(config: &OpenAiConfig) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    let token = format!("Bearer {}", config.api_key);
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&token).context("invalid OPENAI_API_KEY")?,
    );

    if let Some(org) = &config.organization {
        let name = HeaderName::from_static(ORG_HEADER);
        headers.insert(name, HeaderValue::from_str(org)?);
    }

    if let Some(project) = &config.project {
        let name = HeaderName::from_static(PROJECT_HEADER);
        headers.insert(name, HeaderValue::from_str(project)?);
    }

    Ok(headers)
}

fn map_role(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn serialize_message(message: &Message) -> Option<Value> {
    match message.role {
        Role::Tool => {
            let tool_call_id = message.tool_call_id.as_deref()?;
            Some(json!({
                "role": "tool",
                "content": message.content,
                "tool_call_id": tool_call_id,
            }))
        }
        Role::Assistant => {
            let mut payload = json!({
                "role": "assistant",
                "content": message.content,
            });
            if !message.tool_calls.is_empty() {
                let tool_calls = message
                    .tool_calls
                    .iter()
                    .filter(|call| call.call_id.is_some())
                    .map(|call| call.to_openai_tool_call())
                    .collect::<Vec<_>>();
                if !tool_calls.is_empty() {
                    payload["tool_calls"] = Value::Array(tool_calls);
                }
            }
            Some(payload)
        }
        role => Some(json!({
            "role": map_role(role),
            "content": message.content,
        })),
    }
}

fn log_payload(payload: &Value) {
    if env::var("SELENAI_DEBUG_OPENAI").is_ok()
        && let Ok(pretty) = serde_json::to_string_pretty(payload)
    {
        eprintln!("[selenai][openai] payload:\n{}", pretty);
    }
}

fn truncate_payload(text: &str) -> String {
    const LIMIT: usize = 500;
    if text.len() <= LIMIT {
        text.to_string()
    } else {
        format!("{}…", &text[..LIMIT])
    }
}

#[async_trait::async_trait]
impl LlmClient for OpenAiClient {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let payload = self.build_payload(&request, false);
        log_payload(&payload);
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let response = self.http.post(url).json(&payload).send().await?;
        let status = response.status();
        if !status.is_success() {
            let text = response
                .text()
                .await
                .unwrap_or_else(|_| "<failed to read body>".into());
            return Err(anyhow!(
                "OpenAI chat failed (status {}): {}",
                status,
                truncate_payload(&text)
            ));
        }
        let body = response.json::<Value>().await?;
        parse_chat_response(&body)
    }

    async fn chat_stream(&self, request: ChatRequest, sender: StreamEventSender) -> Result<()> {
        let payload = self.build_payload(&request, true);
        log_payload(&payload);
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );

        let response = self.http.post(url).json(&payload).send().await?;
        let status = response.status();
        if !status.is_success() {
            let text = response
                .text()
                .await
                .unwrap_or_else(|_| "<failed to read body>".into());
            return Err(anyhow!(
                "OpenAI chat_stream failed (status {}): {}",
                status,
                truncate_payload(&text)
            ));
        }

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut tool_calls: HashMap<usize, ToolCallState> = HashMap::new();

        while let Some(chunk) = stream.next().await {
            let bytes = chunk?;
            let text = String::from_utf8_lossy(&bytes);
            buffer.push_str(&text);

            while let Some(pos) = buffer.find("\n\n") {
                let mut event = buffer[..pos].to_string();
                buffer.drain(..pos + 2);
                event = event.replace("\r\n", "\n");

                for line in event.lines() {
                    let line = line.trim();
                    if line.is_empty() || !line.starts_with("data:") {
                        continue;
                    }
                    let data = line[5..].trim();
                    if data.is_empty() {
                        continue;
                    }
                    if data == "[DONE]" {
                        finalize_tool_calls(&mut tool_calls, &sender);
                        let _ = sender.send(StreamEvent::Completed);
                        return Ok(());
                    }
                    let json: Value = serde_json::from_str(data)?;
                    handle_stream_chunk(&json, &sender, &mut tool_calls)?;
                }
            }
        }

        finalize_tool_calls(&mut tool_calls, &sender);
        let _ = sender.send(StreamEvent::Completed);
        Ok(())
    }

    fn supports_streaming(&self) -> bool {
        true
    }
}

fn parse_chat_response(value: &Value) -> Result<ChatResponse> {
    let choices = value
        .get("choices")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("missing `choices` in OpenAI response"))?;
    let first = choices
        .first()
        .ok_or_else(|| anyhow!("OpenAI response did not contain any choices"))?;
    let message = first
        .get("message")
        .ok_or_else(|| anyhow!("missing `message` in OpenAI choice"))?;
    parse_message(message)
}

fn parse_message(value: &Value) -> Result<ChatResponse> {
    if let Some(tool_calls) = value.get("tool_calls").and_then(|v| v.as_array())
        && let Some(invocation) = tool_calls.iter().find_map(parse_tool_call)
    {
        return Ok(ChatResponse::ToolCall(invocation));
    }

    let content = value
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    Ok(ChatResponse::assistant_text(content))
}

fn parse_tool_call(value: &Value) -> Option<ToolInvocation> {
    let func = value.get("function")?;
    let name = func.get("name")?.as_str()?.to_string();
    let args_str = func
        .get("arguments")
        .and_then(|v| v.as_str())
        .unwrap_or("{}");
    let arguments = serde_json::from_str(args_str).unwrap_or_else(|_| json!(args_str));
    let call_id = value
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Some(ToolInvocation::from_parts(name, arguments, call_id))
}

fn handle_stream_chunk(
    chunk: &Value,
    sender: &StreamEventSender,
    tool_calls: &mut HashMap<usize, ToolCallState>,
) -> Result<()> {
    let choices = chunk
        .get("choices")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("missing `choices` in OpenAI stream chunk"))?;

    for choice in choices {
        if let Some(delta) = choice.get("delta") {
            if let Some(content) = delta.get("content").and_then(|v| v.as_str())
                && !content.is_empty()
            {
                let _ = sender.send(StreamEvent::Delta(content.to_string()));
            }

            if let Some(tool_list) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                for entry in tool_list {
                    let index = entry.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let state = tool_calls.entry(index).or_default();
                    if let Some(function) = entry.get("function") {
                        if let Some(name) = function.get("name").and_then(|v| v.as_str()) {
                            state.name.get_or_insert_with(|| name.to_string());
                        }
                        if let Some(arguments) = function.get("arguments").and_then(|v| v.as_str())
                        {
                            state.arguments.push_str(arguments);
                        }
                    }
                    if let Some(id) = entry.get("id").and_then(|v| v.as_str()) {
                        state.call_id.get_or_insert_with(|| id.to_string());
                    }
                }
            }
        }

        if let Some(reason) = choice.get("finish_reason").and_then(|v| v.as_str())
            && reason == "tool_calls"
        {
            finalize_tool_calls(tool_calls, sender);
        }
    }

    Ok(())
}

fn finalize_tool_calls(tool_calls: &mut HashMap<usize, ToolCallState>, sender: &StreamEventSender) {
    for state in tool_calls.values() {
        if let Some(name) = &state.name {
            let arguments = serde_json::from_str(&state.arguments)
                .unwrap_or_else(|_| json!(state.arguments.clone()));
            let invocation =
                ToolInvocation::from_parts(name.clone(), arguments, state.call_id.clone());
            let _ = sender.send(StreamEvent::ToolCall(invocation));
        }
    }
    tool_calls.clear();
}

#[derive(Default)]
struct ToolCallState {
    name: Option<String>,
    arguments: String,
    call_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        llm::StreamEvent,
        types::{Message, Role},
    };
    use tokio::sync::mpsc;

    fn test_client() -> OpenAiClient {
        OpenAiClient::new(OpenAiConfig {
            api_key: "test-key".into(),
            model: "test-model".into(),
            base_url: "https://example.test".into(),
            organization: None,
            project: None,
        })
        .expect("client")
    }

    #[test]
    fn payload_includes_system_prompt() {
        let client = test_client();
        let request = ChatRequest::new(vec![Message::new(Role::User, "ping")])
            .with_system_prompt("system instructions");
        let payload = client.build_payload(&request, false);
        let messages = payload
            .get("messages")
            .and_then(|v| v.as_array())
            .expect("messages");
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "system instructions");
        assert_eq!(messages[1]["role"], "user");
    }

    #[test]
    fn payload_includes_tools() {
        let client = test_client();
        let tool = LlmTool::new(
            "lua_run_script",
            "Run Lua script",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "source": { "type": "string" }
                },
                "required": ["source"]
            }),
        );
        let request = ChatRequest::new(vec![Message::new(Role::User, "ping")]).with_tool(tool);
        let payload = client.build_payload(&request, false);
        let tools = payload
            .get("tools")
            .and_then(|v| v.as_array())
            .expect("tools");
        assert_eq!(tools[0]["function"]["name"], "lua_run_script");
    }

    #[test]
    fn payload_skips_tool_messages_without_id() {
        let client = test_client();
        let mut request = ChatRequest::new(vec![Message::new(Role::User, "ping")]);
        request
            .messages
            .push(Message::new(Role::Tool, "manual tool output"));
        let payload = client.build_payload(&request, false);
        let messages = payload
            .get("messages")
            .and_then(|v| v.as_array())
            .expect("messages");
        assert_eq!(
            messages.len(),
            1,
            "tool message without id should be skipped"
        );
    }

    #[test]
    fn payload_includes_tool_messages_with_id() {
        let client = test_client();
        let mut request = ChatRequest::new(vec![Message::new(Role::User, "ping")]);
        request
            .messages
            .push(Message::new_tool("call_123", "result output"));
        let payload = client.build_payload(&request, false);
        let messages = payload
            .get("messages")
            .and_then(|v| v.as_array())
            .expect("messages");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["role"], "tool");
        assert_eq!(messages[1]["tool_call_id"], "call_123");
        assert_eq!(messages[1]["content"], "result output");
    }

    #[test]
    fn payload_includes_assistant_tool_call_metadata() {
        let client = test_client();
        let mut request = ChatRequest::new(vec![Message::new(Role::User, "ping")]);
        let invocation = ToolInvocation::from_parts(
            "lua_run_script",
            serde_json::json!({"source": "return 1"}),
            Some("call_456".into()),
        );
        let mut assistant = Message::new(Role::Assistant, "Queuing tool run");
        assistant.tool_calls.push(invocation);
        request.messages.push(assistant);
        let payload = client.build_payload(&request, false);
        let messages = payload
            .get("messages")
            .and_then(|v| v.as_array())
            .expect("messages");
        assert_eq!(messages.len(), 2);
        assert!(messages[1].get("tool_calls").is_some());
    }

    #[test]
    fn parse_chat_response_returns_plain_text() {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Hello!"
                }
            }]
        });
        let response = parse_chat_response(&body).expect("parsed");
        match response {
            ChatResponse::Assistant(message) => assert_eq!(message.content, "Hello!"),
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[test]
    fn parse_chat_response_yields_tool_call() {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "lua_run_script",
                            "arguments": "{\"source\":\"return 1\"}"
                        }
                    }]
                }
            }]
        });
        let response = parse_chat_response(&body).expect("parsed");
        match response {
            ChatResponse::ToolCall(invocation) => {
                assert_eq!(invocation.name, "lua_run_script");
                assert_eq!(invocation.call_id.as_deref(), Some("call_1"));
                assert_eq!(invocation.arguments["source"], "return 1");
            }
            other => panic!("expected tool call, got {other:?}"),
        }
    }

    #[test]
    fn handle_stream_chunk_emits_events() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let chunk = serde_json::json!({
            "choices": [{
                "delta": {
                    "content": "Hello",
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_99",
                        "function": {
                            "name": "lua_run_script",
                            "arguments": "{\"source\":\"return 1\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let mut tool_state: std::collections::HashMap<usize, ToolCallState> =
            std::collections::HashMap::new();
        handle_stream_chunk(&chunk, &tx, &mut tool_state).expect("stream chunk");
        let first = rx.try_recv().expect("delta event");
        match first {
            StreamEvent::Delta(text) => assert_eq!(text, "Hello"),
            other => panic!("expected delta, got {other:?}"),
        }
        let second = rx.try_recv().expect("tool call event");
        match second {
            StreamEvent::ToolCall(invocation) => {
                assert_eq!(invocation.name, "lua_run_script");
                assert_eq!(invocation.call_id.as_deref(), Some("call_99"));
            }
            other => panic!("expected tool call, got {other:?}"),
        }
    }
}
