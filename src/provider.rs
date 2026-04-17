use crate::config::{Config, ProviderKind, ThinkMode};
use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

pub enum StreamEvent<'a> {
    Content(&'a str),
    Thinking(&'a str),
}

pub struct ProviderClient {
    http: Client,
    kind: ProviderKind,
    api_key: Option<String>,
    base_url: String,
    model: String,
    think: ThinkMode,
    stop_sequences: Vec<String>,
}

impl ProviderClient {
    pub fn new(config: &Config) -> Self {
        Self {
            http: Client::new(),
            kind: config.provider,
            api_key: config.api_key.clone(),
            base_url: config.base_url().to_string(),
            model: config.model().to_string(),
            think: config.think,
            stop_sequences: config.stop_sequences.clone(),
        }
    }

    #[allow(dead_code)]
    pub async fn complete(&self, messages: &[Message]) -> Result<String> {
        match self.kind {
            ProviderKind::Openai | ProviderKind::Openrouter | ProviderKind::CustomOpenai => {
                self.openai_compatible(messages).await
            }
            ProviderKind::Anthropic => self.anthropic(messages).await,
            ProviderKind::Gemini => self.gemini(messages).await,
            ProviderKind::Ollama => self.ollama(messages).await,
        }
    }

    pub async fn complete_stream<F>(&self, messages: &[Message], mut on_delta: F) -> Result<String>
    where
        F: FnMut(StreamEvent<'_>) -> Result<()>,
    {
        match self.kind {
            ProviderKind::Openai | ProviderKind::Openrouter | ProviderKind::CustomOpenai => {
                self.openai_compatible_stream(messages, &mut on_delta).await
            }
            ProviderKind::Anthropic => self.anthropic_stream(messages, &mut on_delta).await,
            ProviderKind::Gemini => self.gemini_stream(messages, &mut on_delta).await,
            ProviderKind::Ollama => self.ollama_stream(messages, &mut on_delta).await,
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn set_model(&mut self, model: String) {
        self.model = model;
    }

    pub fn think(&self) -> ThinkMode {
        self.think
    }

    pub fn set_think(&mut self, think: ThinkMode) {
        self.think = think;
    }

    pub fn stop_sequences(&self) -> &[String] {
        &self.stop_sequences
    }

    pub fn set_stop_sequences(&mut self, stop_sequences: Vec<String>) {
        self.stop_sequences = stop_sequences;
    }

    #[allow(dead_code)]
    async fn openai_compatible(&self, messages: &[Message]) -> Result<String> {
        let mut body = json!({
            "model": self.model,
            "messages": messages.iter().map(|m| json!({
                "role": role_name(&m.role),
                "content": m.content
            })).collect::<Vec<_>>(),
            "temperature": 0.2
        });
        self.add_openai_stop(&mut body);

        let mut request = self.http.post(&self.base_url).json(&body);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        if matches!(self.kind, ProviderKind::Openrouter) {
            request = request
                .header("HTTP-Referer", "https://localhost")
                .header("X-Title", "coding-agent-rs");
        }

        let value: Value = request
            .send()
            .await
            .context("provider request failed")?
            .error_for_status()
            .context("provider returned an error")?
            .json()
            .await
            .context("provider response was not JSON")?;

        value["choices"][0]["message"]["content"]
            .as_str()
            .map(ToOwned::to_owned)
            .ok_or_else(|| anyhow!("unexpected OpenAI-compatible response: {value}"))
    }

    async fn openai_compatible_stream<F>(
        &self,
        messages: &[Message],
        on_delta: &mut F,
    ) -> Result<String>
    where
        F: FnMut(StreamEvent<'_>) -> Result<()>,
    {
        let mut body = json!({
            "model": self.model,
            "stream": true,
            "messages": messages.iter().map(|m| json!({
                "role": role_name(&m.role),
                "content": m.content
            })).collect::<Vec<_>>(),
            "temperature": 0.2
        });
        self.add_openai_stop(&mut body);

        let mut request = self.http.post(&self.base_url).json(&body);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        if matches!(self.kind, ProviderKind::Openrouter) {
            request = request
                .header("HTTP-Referer", "https://localhost")
                .header("X-Title", "coding-agent-rs");
        }

        let response = request
            .send()
            .await
            .context("provider streaming request failed")?
            .error_for_status()
            .context("provider streaming request returned an error")?;

        collect_line_stream(response, |line, answer| {
            let Some(data) = line.strip_prefix("data:") else {
                return Ok(false);
            };
            let data = data.trim();
            if data == "[DONE]" {
                return Ok(true);
            }
            if data.is_empty() {
                return Ok(false);
            }
            let value: Value = serde_json::from_str(data)
                .with_context(|| format!("invalid OpenAI-compatible stream event: {data}"))?;
            if let Some(delta) = value["choices"][0]["delta"]["content"].as_str() {
                answer.push_str(delta);
                on_delta(StreamEvent::Content(delta))?;
            }
            Ok(false)
        })
        .await
    }

    #[allow(dead_code)]
    async fn anthropic(&self, messages: &[Message]) -> Result<String> {
        let system = messages
            .iter()
            .filter(|m| matches!(m.role, Role::System))
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");

        let chat = messages
            .iter()
            .filter(|m| !matches!(m.role, Role::System))
            .map(|m| {
                json!({
                    "role": match m.role {
                        Role::Assistant => "assistant",
                        _ => "user",
                    },
                    "content": m.content
                })
            })
            .collect::<Vec<_>>();

        let mut body = json!({
            "model": self.model,
            "max_tokens": 4096,
            "system": system,
            "messages": chat
        });
        self.add_anthropic_stop(&mut body);

        let value: Value = self
            .http
            .post(&self.base_url)
            .header("x-api-key", self.api_key.as_deref().unwrap_or_default())
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .context("Anthropic request failed")?
            .error_for_status()
            .context("Anthropic returned an error")?
            .json()
            .await
            .context("Anthropic response was not JSON")?;

        value["content"]
            .as_array()
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .filter(|text| !text.is_empty())
            .ok_or_else(|| anyhow!("unexpected Anthropic response: {value}"))
    }

    async fn anthropic_stream<F>(&self, messages: &[Message], on_delta: &mut F) -> Result<String>
    where
        F: FnMut(StreamEvent<'_>) -> Result<()>,
    {
        let system = messages
            .iter()
            .filter(|m| matches!(m.role, Role::System))
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");

        let chat = messages
            .iter()
            .filter(|m| !matches!(m.role, Role::System))
            .map(|m| {
                json!({
                    "role": match m.role {
                        Role::Assistant => "assistant",
                        _ => "user",
                    },
                    "content": m.content
                })
            })
            .collect::<Vec<_>>();

        let mut body = json!({
            "model": self.model,
            "max_tokens": 4096,
            "stream": true,
            "system": system,
            "messages": chat
        });
        self.add_anthropic_stop(&mut body);

        let response = self
            .http
            .post(&self.base_url)
            .header("x-api-key", self.api_key.as_deref().unwrap_or_default())
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .context("Anthropic streaming request failed")?
            .error_for_status()
            .context("Anthropic streaming request returned an error")?;

        collect_line_stream(response, |line, answer| {
            let Some(data) = line.strip_prefix("data:") else {
                return Ok(false);
            };
            let data = data.trim();
            if data == "[DONE]" || data.is_empty() {
                return Ok(false);
            }
            let value: Value = serde_json::from_str(data)
                .with_context(|| format!("invalid Anthropic stream event: {data}"))?;
            if value["type"].as_str() == Some("message_stop") {
                return Ok(true);
            }
            if let Some(delta) = value["delta"]["text"].as_str() {
                answer.push_str(delta);
                on_delta(StreamEvent::Content(delta))?;
            }
            Ok(false)
        })
        .await
    }

    #[allow(dead_code)]
    async fn gemini(&self, messages: &[Message]) -> Result<String> {
        let prompt = messages
            .iter()
            .map(|m| format!("{}: {}", role_name(&m.role), m.content))
            .collect::<Vec<_>>()
            .join("\n\n");
        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.base_url.trim_end_matches('/'),
            self.model,
            self.api_key.as_deref().unwrap_or_default()
        );
        let mut body = json!({
            "contents": [{
                "role": "user",
                "parts": [{ "text": prompt }]
            }],
            "generationConfig": { "temperature": 0.2 }
        });
        self.add_gemini_stop(&mut body);

        let value: Value = self
            .http
            .post(url)
            .json(&body)
            .send()
            .await
            .context("Gemini request failed")?
            .error_for_status()
            .context("Gemini returned an error")?
            .json()
            .await
            .context("Gemini response was not JSON")?;

        value["candidates"][0]["content"]["parts"]
            .as_array()
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .filter(|text| !text.is_empty())
            .ok_or_else(|| anyhow!("unexpected Gemini response: {value}"))
    }

    async fn gemini_stream<F>(&self, messages: &[Message], on_delta: &mut F) -> Result<String>
    where
        F: FnMut(StreamEvent<'_>) -> Result<()>,
    {
        let prompt = messages
            .iter()
            .map(|m| format!("{}: {}", role_name(&m.role), m.content))
            .collect::<Vec<_>>()
            .join("\n\n");
        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse&key={}",
            self.base_url.trim_end_matches('/'),
            self.model,
            self.api_key.as_deref().unwrap_or_default()
        );
        let mut body = json!({
            "contents": [{
                "role": "user",
                "parts": [{ "text": prompt }]
            }],
            "generationConfig": { "temperature": 0.2 }
        });
        self.add_gemini_stop(&mut body);

        let response = self
            .http
            .post(url)
            .json(&body)
            .send()
            .await
            .context("Gemini streaming request failed")?
            .error_for_status()
            .context("Gemini streaming request returned an error")?;

        collect_line_stream(response, |line, answer| {
            let Some(data) = line.strip_prefix("data:") else {
                return Ok(false);
            };
            let data = data.trim();
            if data.is_empty() {
                return Ok(false);
            }
            let value: Value = serde_json::from_str(data)
                .with_context(|| format!("invalid Gemini stream event: {data}"))?;
            if let Some(parts) = value["candidates"][0]["content"]["parts"].as_array() {
                for part in parts {
                    if let Some(delta) = part["text"].as_str() {
                        answer.push_str(delta);
                        on_delta(StreamEvent::Content(delta))?;
                    }
                }
            }
            Ok(false)
        })
        .await
    }

    #[allow(dead_code)]
    async fn ollama(&self, messages: &[Message]) -> Result<String> {
        let mut body = json!({
            "model": self.model,
            "stream": false,
            "messages": messages.iter().map(|m| json!({
                "role": role_name(&m.role),
                "content": m.content
            })).collect::<Vec<_>>()
        });
        self.add_ollama_controls(&mut body);

        let value: Value = self
            .http
            .post(&self.base_url)
            .json(&body)
            .send()
            .await
            .context("Ollama request failed")?
            .error_for_status()
            .context("Ollama returned an error")?
            .json()
            .await
            .context("Ollama response was not JSON")?;

        value["message"]["content"]
            .as_str()
            .map(ToOwned::to_owned)
            .ok_or_else(|| anyhow!("unexpected Ollama response: {value}"))
    }

    async fn ollama_stream<F>(&self, messages: &[Message], on_delta: &mut F) -> Result<String>
    where
        F: FnMut(StreamEvent<'_>) -> Result<()>,
    {
        let mut body = json!({
            "model": self.model,
            "stream": true,
            "messages": messages.iter().map(|m| json!({
                "role": role_name(&m.role),
                "content": m.content
            })).collect::<Vec<_>>()
        });
        self.add_ollama_controls(&mut body);

        let response = self
            .http
            .post(&self.base_url)
            .json(&body)
            .send()
            .await
            .context("Ollama streaming request failed")?
            .error_for_status()
            .context("Ollama streaming request returned an error")?;

        collect_line_stream(response, |line, answer| {
            if line.trim().is_empty() {
                return Ok(false);
            }
            let value: Value = serde_json::from_str(line)
                .with_context(|| format!("invalid Ollama stream event: {line}"))?;
            if let Some(delta) = value["message"]["content"].as_str() {
                answer.push_str(delta);
                on_delta(StreamEvent::Content(delta))?;
            }
            if let Some(thinking) = value["message"]["thinking"].as_str() {
                on_delta(StreamEvent::Thinking(thinking))?;
            }
            Ok(value["done"].as_bool().unwrap_or(false))
        })
        .await
    }

    fn add_openai_stop(&self, body: &mut Value) {
        if !self.stop_sequences.is_empty() {
            body["stop"] = json!(self.stop_sequences);
        }
    }

    fn add_anthropic_stop(&self, body: &mut Value) {
        if !self.stop_sequences.is_empty() {
            body["stop_sequences"] = json!(self.stop_sequences);
        }
    }

    fn add_gemini_stop(&self, body: &mut Value) {
        if !self.stop_sequences.is_empty() {
            body["generationConfig"]["stopSequences"] = json!(self.stop_sequences);
        }
    }

    fn add_ollama_controls(&self, body: &mut Value) {
        if let Some(think) = self.think.as_request_value() {
            body["think"] = think;
        }
        if !self.stop_sequences.is_empty() {
            body["options"] = json!({ "stop": self.stop_sequences });
        }
    }
}

async fn collect_line_stream<F>(
    mut response: reqwest::Response,
    mut handle_line: F,
) -> Result<String>
where
    F: FnMut(&str, &mut String) -> Result<bool>,
{
    let mut answer = String::new();
    let mut buffered = String::new();

    while let Some(chunk) = response
        .chunk()
        .await
        .context("failed to read streaming chunk")?
    {
        buffered.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(newline) = buffered.find('\n') {
            let line = buffered[..newline].trim_end_matches('\r').to_string();
            buffered.drain(..=newline);
            if handle_line(&line, &mut answer)? {
                return Ok(answer);
            }
        }
    }

    if !buffered.trim().is_empty() {
        handle_line(buffered.trim_end_matches('\r'), &mut answer)?;
    }

    Ok(answer)
}

fn role_name(role: &Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}
