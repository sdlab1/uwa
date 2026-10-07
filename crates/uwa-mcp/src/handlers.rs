//! Built-in MCP handlers exposing the bridge itself.

use crate::protocol::{
    CallToolResult, GetPromptResult, McpContent, McpPrompt, McpPromptArgument, McpResource,
    McpTool, PromptMessage, ReadResourceResult, ResourceContents,
};
use crate::server::McpHandler;
use async_trait::async_trait;
use futures::future::BoxFuture;
use serde_json::{json, Value};
use std::sync::Arc;
use uwa_core::{Result, Transport, UwaError};

/// A chat call is `dispatch(provider, message) -> reply`. The closure owns
/// whatever it needs (state, sessions, breaker), which keeps `uwa-mcp` free of
/// any dependency on `uwa-api`.
pub type DispatcherFn =
    Arc<dyn Fn(String, String) -> BoxFuture<'static, Result<String>> + Send + Sync>;

/// `web_chat(provider, message)` — one-shot ask to a web-UI LLM.
pub struct WebChatHandler {
    dispatch: DispatcherFn,
}

impl WebChatHandler {
    pub fn new(dispatch: DispatcherFn) -> Self {
        Self { dispatch }
    }
}

#[async_trait]
impl McpHandler for WebChatHandler {
    fn namespace(&self) -> &str {
        "web"
    }

    fn tools(&self) -> Vec<McpTool> {
        vec![McpTool {
            name: "web__chat".into(),
            description: "Send a message to a logged-in web LLM (chatgpt/claude/gemini/deepseek) and return its reply.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "provider": {"type": "string", "description": "e.g. chatgpt, claude"},
                    "message":  {"type": "string"}
                },
                "required": ["provider", "message"]
            }),
        }]
    }

    async fn call(&self, tool: &str, args: Value) -> Result<CallToolResult> {
        if tool != "chat" {
            return Err(UwaError::BadRequest(format!("unknown tool `web__{tool}`")));
        }
        let provider_name = args
            .get("provider")
            .and_then(Value::as_str)
            .ok_or_else(|| UwaError::BadRequest("missing `provider`".into()))?;
        let message = args
            .get("message")
            .and_then(Value::as_str)
            .ok_or_else(|| UwaError::BadRequest("missing `message`".into()))?;

        let answer = (self.dispatch)(provider_name.to_string(), message.to_string()).await?;
        Ok(CallToolResult {
            content: vec![McpContent::Text { text: answer }],
            is_error: false,
        })
    }
}

/// Resource listing of the connected tabs.
pub const TABS_URI: &str = "uwa://web/tabs";

/// `web_tabs_list()` + `uwa://web/tabs` — health/status.
pub struct WebTabsHandler {
    transport: Arc<dyn Transport>,
}

impl WebTabsHandler {
    pub fn new(transport: Arc<dyn Transport>) -> Self {
        Self { transport }
    }
}

#[async_trait]
impl McpHandler for WebTabsHandler {
    fn namespace(&self) -> &str {
        "web"
    }

    fn tools(&self) -> Vec<McpTool> {
        vec![McpTool {
            name: "web__list_tabs".into(),
            description: "List currently connected browser tabs.".into(),
            input_schema: json!({"type": "object", "properties": {}}),
        }]
    }

    async fn call(&self, tool: &str, _args: Value) -> Result<CallToolResult> {
        if tool != "list_tabs" {
            return Err(UwaError::BadRequest(format!("unknown tool `web__{tool}`")));
        }
        let text = self.tabs_json().await?;
        Ok(CallToolResult::text(text))
    }

    fn resources(&self) -> Vec<McpResource> {
        vec![McpResource {
            uri: TABS_URI.into(),
            name: "tabs".into(),
            description: Some("Currently connected browser tabs as a JSON array.".into()),
            mime_type: Some("application/json".into()),
        }]
    }

    async fn read_resource(&self, uri: &str) -> Result<ReadResourceResult> {
        if uri != TABS_URI {
            return Err(UwaError::BadRequest(format!("resource `{uri}` not found")));
        }
        Ok(ReadResourceResult {
            contents: vec![ResourceContents {
                uri: TABS_URI.into(),
                mime_type: Some("application/json".into()),
                text: Some(self.tabs_json().await?),
                blob: None,
            }],
        })
    }
}

impl WebTabsHandler {
    async fn tabs_json(&self) -> Result<String> {
        let tabs = self.transport.list_tabs().await?;
        Ok(
            serde_json::to_string(&tabs.iter().map(|t| t.as_str()).collect::<Vec<_>>())
                .unwrap_or_else(|_| "[]".into()),
        )
    }
}

/// Providers the daemon knows about, for prompt descriptions and validation.
pub type ProvidersFn = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// Prompt `ask` — the client renders it into a user message and sends it
/// itself; no browser round trip happens here.
pub struct WebPromptHandler {
    providers: ProvidersFn,
}

impl WebPromptHandler {
    pub fn new(providers: ProvidersFn) -> Self {
        Self { providers }
    }
}

#[async_trait]
impl McpHandler for WebPromptHandler {
    fn namespace(&self) -> &str {
        "web"
    }

    fn prompts(&self) -> Vec<McpPrompt> {
        let names = (self.providers)();
        let description = if names.is_empty() {
            "Ask a web LLM a question.".to_string()
        } else {
            format!(
                "Ask one of the configured web LLMs ({}) a question.",
                names.join(", ")
            )
        };
        vec![McpPrompt {
            name: "ask".into(),
            description: Some(description),
            arguments: vec![
                McpPromptArgument {
                    name: "question".into(),
                    description: Some("Question to ask.".into()),
                    required: Some(true),
                },
                McpPromptArgument {
                    name: "provider".into(),
                    description: Some("Provider name; defaults to the only configured one.".into()),
                    required: Some(false),
                },
            ],
        }]
    }

    async fn get_prompt(&self, name: &str, args: Value) -> Result<GetPromptResult> {
        if name != "ask" {
            return Err(UwaError::BadRequest(format!("prompt `{name}` not found")));
        }
        let question = args
            .get("question")
            .and_then(Value::as_str)
            .filter(|q| !q.is_empty())
            .ok_or_else(|| UwaError::BadRequest("missing `question`".into()))?;
        if let Some(provider) = args.get("provider").and_then(Value::as_str) {
            let known = (self.providers)();
            if !known.iter().any(|p| p == provider) {
                return Err(UwaError::BadRequest(format!(
                    "unknown provider `{provider}`"
                )));
            }
        }
        Ok(GetPromptResult {
            description: Some(question.to_string()),
            messages: vec![PromptMessage {
                role: "user".into(),
                content: McpContent::Text {
                    text: question.to_string(),
                },
            }],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uwa_testkit::MockTransport;

    #[tokio::test]
    async fn tabs_handler_reads_the_tabs_resource() {
        let tabs = MockTransport::with_n_tabs(1);
        let id = tabs.tab_ids()[0].to_string();
        let h = WebTabsHandler::new(Arc::new(tabs));
        assert_eq!(h.resources()[0].uri, TABS_URI);
        let r = h.read_resource(TABS_URI).await.unwrap();
        assert_eq!(r.contents[0].mime_type.as_deref(), Some("application/json"));
        assert!(
            r.contents[0].text.as_ref().unwrap().contains(&id),
            "missing {id}"
        );
        assert!(h.read_resource("uwa://nope").await.is_err());
    }

    #[tokio::test]
    async fn ask_prompt_renders_the_question() {
        let h = WebPromptHandler::new(Arc::new(|| vec!["chatgpt".into(), "claude".into()]));
        let prompt = &h.prompts()[0];
        assert_eq!(prompt.name, "ask");
        assert!(
            prompt
                .description
                .as_deref()
                .is_some_and(|d| d.contains("chatgpt, claude")),
            "description: {:?}",
            prompt.description
        );

        let r = h
            .get_prompt("ask", json!({"question": "what now?"}))
            .await
            .unwrap();
        assert_eq!(r.messages.len(), 1);
        assert_eq!(r.messages[0].role, "user");

        let ok = h
            .get_prompt("ask", json!({"question": "hi", "provider": "chatgpt"}))
            .await;
        assert!(ok.is_ok());
        let unknown = h
            .get_prompt("ask", json!({"question": "hi", "provider": "nope"}))
            .await;
        assert!(unknown.is_err());
        assert!(h.get_prompt("ask", json!({})).await.is_err());
        assert!(h
            .get_prompt("nope", json!({"question": "x"}))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn chat_handler_delegates_to_the_dispatcher() {
        let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(vec![]));
        let seen2 = seen.clone();
        let handler = WebChatHandler::new(Arc::new(move |provider, message| {
            let seen = seen2.clone();
            Box::pin(async move {
                seen.lock().unwrap().push(format!("{provider}:{message}"));
                Ok(format!("echo {message}"))
            })
        }));
        let out = handler
            .call("chat", json!({"provider": "chatgpt", "message": "hi"}))
            .await
            .unwrap();
        assert_eq!(out.as_text(), "echo hi");
        assert_eq!(&seen.lock().unwrap()[..], &["chatgpt:hi".to_string()]);
        assert!(handler.call("other", json!({})).await.is_err());
    }

    #[tokio::test]
    async fn handlers_may_implement_only_part_of_the_surface() {
        struct Minimal;
        #[async_trait]
        impl McpHandler for Minimal {
            fn namespace(&self) -> &str {
                "min"
            }
        }
        let h = Minimal;
        assert!(h.tools().is_empty());
        assert!(h.resources().is_empty());
        assert!(h.prompts().is_empty());
        assert!(h.call("x", json!({})).await.is_err());
        assert!(h.read_resource("uwa://x").await.is_err());
        assert!(h.get_prompt("x", json!({})).await.is_err());
    }
}
