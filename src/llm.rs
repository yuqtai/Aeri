//! Provider clients with an Anthropic-style message boundary for the agent.

use std::time::Duration;

use anyhow::{Context, bail};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiStyle { Anthropic, OpenAi }

pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub url: &'static str,
    pub model: &'static str,
    pub env: &'static str,
    pub style: ApiStyle,
}

pub const PRESETS: &[Preset] = &[
    Preset { id: "openai", name: "OpenAI", url: "https://api.openai.com/v1", model: "gpt-4.1-mini", env: "OPENAI_API_KEY", style: ApiStyle::OpenAi },
    Preset { id: "anthropic", name: "Anthropic", url: "https://api.anthropic.com", model: "claude-sonnet-5-5", env: "ANTHROPIC_API_KEY", style: ApiStyle::Anthropic },
    Preset { id: "gemini", name: "Google Gemini", url: "https://generativelanguage.googleapis.com/v1beta/openai", model: "gemini-3.8-flash", env: "GEMINI_API_KEY", style: ApiStyle::OpenAi },
    Preset { id: "openrouter", name: "OpenRouter", url: "https://openrouter.ai/api/v1", model: "openai/gpt-4.1-mini", env: "OPENROUTER_API_KEY", style: ApiStyle::OpenAi },
    Preset { id: "groq", name: "Groq", url: "https://api.groq.com/openai/v1", model: "openai/gpt-oss-120b", env: "GROQ_API_KEY", style: ApiStyle::OpenAi },
    Preset { id: "deepseek", name: "DeepSeek", url: "https://api.deepseek.com/anthropic", model: "deepseek-flash", env: "DEEPSEEK_API_KEY", style: ApiStyle::Anthropic },
    Preset { id: "ollama", name: "Ollama (local)", url: "http://localhost:11434/v1", model: "qwen3", env: "", style: ApiStyle::OpenAi },
    Preset { id: "custom", name: "Custom", url: "http://localhost:8000/v1", model: "default", env: "", style: ApiStyle::OpenAi },
];

pub fn preset(id: &str) -> &Preset {
    PRESETS.iter().find(|p| p.id == id).unwrap_or(&PRESETS[7])
}

pub struct Llm {
    http: reqwest::Client,
    url: String,
    api_key: String,
    model: String,
    style: ApiStyle,
    deepseek: bool,
    thinking: bool,
    effort: String,
}

impl Llm {
    pub fn new(provider: &str, base_url: &str, api_key: String, model: &str, thinking: bool, effort: &str) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(120)).build()?;
        let style = preset(provider).style;
        let base = base_url.trim_end_matches('/');
        let url = match style {
            ApiStyle::Anthropic => format!("{}/v1/messages", base.strip_suffix("/v1").unwrap_or(base)),
            ApiStyle::OpenAi => if base.ends_with("/chat/completions") { base.to_string() } else { format!("{base}/chat/completions") },
        };
        Ok(Self {
            http,
            url,
            style,
            deepseek: provider == "deepseek",
            api_key,
            model: model.to_string(),
            thinking,
            effort: effort.to_string(),
        })
    }

    /// 发一次请求，返回完整的响应 JSON（content 原样保留，思考块要原样回传）
    pub async fn create(&self, system: &str, tools: &[Value], messages: &[Value]) -> anyhow::Result<Value> {
        if self.style == ApiStyle::OpenAi {
            let body = chat_request(&self.model, system, tools, messages)?;
            return chat_response(&self.send(&body).await?);
        }
        let mut body = json!({
            "model": self.model,
            "max_tokens": 8000,
            "system": system,
            "tools": tools,
            "messages": messages,
        });
        if self.thinking {
            body["thinking"] = if self.deepseek { json!({"type": "enabled"}) } else { json!({"type": "adaptive"}) };
            body["output_config"] = json!({"effort": if !self.deepseek && self.effort == "max" { "high" } else { &self.effort }});
        } else {
            body["thinking"] = json!({"type": "disabled"});
        }
        self.send(&body).await
    }

    /// 测试 key、地址和模型名：发一条极短的请求
    pub async fn ping(&self) -> anyhow::Result<()> {
        if self.style == ApiStyle::OpenAi {
            return self.create("", &[], &[json!({"role": "user", "content": "Reply OK."})]).await.map(|_| ());
        }
        let body = json!({
            "model": self.model,
            "max_tokens": 16,
            "thinking": {"type": "disabled"},
            "messages": [{"role": "user", "content": "你好"}],
        });
        self.send(&body).await.map(|_| ())
    }

    async fn send(&self, body: &Value) -> anyhow::Result<Value> {
        let mut request = self.http.post(&self.url);
        if !self.api_key.is_empty() { request = request.bearer_auth(&self.api_key); }
        if self.style == ApiStyle::Anthropic {
            request = request.header("x-api-key", &self.api_key).header("anthropic-version", "2023-06-01");
        }
        let resp = request
            .json(body)
            .send()
            .await
            .context("请求大模型失败（检查网络）")?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            let detail: String = text.chars().take(300).collect();
            match status.as_u16() {
                401 | 403 => bail!("大模型的 API Key 无效或已过期，请在“设置”里更新（{status}）"),
                402 => bail!("大模型账户余额不足，请充值后再试（{status}）"),
                429 => bail!("大模型请求太频繁或额度用完了，请稍后再试（{status}）"),
                500..=599 => bail!("大模型服务暂时不可用，请稍后再试（{status}）"),
                _ => bail!("大模型返回 {status}：{detail}"),
            }
        }
        Ok(serde_json::from_str(&text)?)
    }
}

// Translate the agent's Anthropic message boundary without exposing provider details.
fn chat_request(model: &str, system: &str, tools: &[Value], messages: &[Value]) -> anyhow::Result<Value> {
    let mut out = Vec::new();
    if !system.is_empty() { out.push(json!({"role": "system", "content": system})); }
    for m in messages {
        if m["content"].is_string() { out.push(m.clone()); continue; }
        let blocks = m["content"].as_array().context("Expected message content blocks")?;
        let mut text = Vec::new();
        let mut calls = Vec::new();
        let mut results = Vec::new();
        for b in blocks {
            match b["type"].as_str() {
                Some("text") => text.push(b["text"].as_str().unwrap_or("")),
                Some("tool_use") => {
                    let mut call = b.get("_openai_tool_call").cloned().unwrap_or_else(|| json!({}));
                    call["id"] = b["id"].clone();
                    call["type"] = json!("function");
                    call["function"]["name"] = b["name"].clone();
                    call["function"]["arguments"] = json!(b["input"].to_string());
                    calls.push(call);
                },
                Some("tool_result") => {
                    let content = if let Some(s) = b["content"].as_str() { s.to_string() } else { b["content"].to_string() };
                    results.push(json!({"role": "tool", "tool_call_id": b["tool_use_id"], "content": content}));
                }
                _ => {}
            }
        }
        out.extend(results);
        if !text.is_empty() || !calls.is_empty() {
            let mut msg = json!({"role": m["role"], "content": if text.is_empty() { Value::Null } else { json!(text.join("\n")) }});
            if !calls.is_empty() { msg["tool_calls"] = json!(calls); }
            for key in ["reasoning_content", "reasoning"] {
                if let Some(value) = blocks.iter().find_map(|b| b.get(format!("_openai_{key}"))) { msg[key] = value.clone(); }
            }
            out.push(msg);
        }
    }
    let mut body = json!({"model": model, "messages": out});
    if !tools.is_empty() {
        body["tools"] = json!(tools.iter().map(|t| json!({"type": "function", "function": {"name": t["name"], "description": t["description"], "parameters": t["input_schema"]}})).collect::<Vec<_>>());
    }
    Ok(body)
}

fn chat_response(response: &Value) -> anyhow::Result<Value> {
    let choice = response["choices"].get(0).context("No chat completion choice returned")?;
    let msg = &choice["message"];
    let mut content = Vec::new();
    if let Some(text) = msg["content"].as_str().filter(|s| !s.is_empty()) { content.push(json!({"type": "text", "text": text})); }
    if let Some(calls) = msg["tool_calls"].as_array() {
        for call in calls {
            let input: Value = serde_json::from_str(call["function"]["arguments"].as_str().context("Missing tool arguments")?).context("Invalid tool arguments JSON")?;
            // Gemini and other providers attach opaque tool-call metadata such as
            // thought signatures; carry it through the agent's block history.
            content.push(json!({"type": "tool_use", "id": call["id"], "name": call["function"]["name"], "input": input, "_openai_tool_call": call}));
        }
    }
    if let Some(first) = content.first_mut() {
        for key in ["reasoning_content", "reasoning"] {
            if let Some(value) = msg.get(key) { first[format!("_openai_{key}")] = value.clone(); }
        }
    }
    let stop = match choice["finish_reason"].as_str() { Some("tool_calls") => "tool_use", Some("length") => "max_tokens", _ => "end_turn" };
    Ok(json!({"content": content, "stop_reason": stop}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recorded_tool_roundtrip() {
        let mut sample: Value = serde_json::from_str(r#"{"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":"Checking.","tool_calls":[{"id":"call_1","type":"function","function":{"name":"weather","arguments":"{\"city\":\"London\"}"}}]}}]}"#).unwrap();
        sample["choices"][0]["message"]["tool_calls"][0]["extra_content"] = json!({"google":{"thought_signature":"recorded-signature"}});
        sample["choices"][0]["message"]["reasoning_content"] = json!("Recorded reasoning");
        let converted = chat_response(&sample).unwrap();
        assert_eq!(converted["stop_reason"], "tool_use");
        assert_eq!(converted["content"][1]["input"]["city"], "London");
        let body = chat_request("m", "system", &[json!({"name":"weather","description":"Weather","input_schema":{"type":"object"}})], &[
            json!({"role":"user","content":"Weather?"}),
            json!({"role":"assistant","content":converted["content"]}),
            json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"call_1","content":"Rain","is_error":false},{"type":"text","text":"Thanks"}]})
        ]).unwrap();
        assert_eq!(body["messages"][2]["tool_calls"], sample["choices"][0]["message"]["tool_calls"]);
        assert_eq!(body["messages"][2]["reasoning_content"], "Recorded reasoning");
        assert_eq!(body["messages"][3], json!({"role":"tool","tool_call_id":"call_1","content":"Rain"}));
        assert_eq!(body["messages"][4]["content"], "Thanks");
        assert_eq!(body["tools"][0]["function"]["parameters"]["type"], "object");
    }
    #[test]
    fn multiple_tool_results_and_empty_assistant_text() {
        let response = chat_response(&json!({"choices":[{"finish_reason":"tool_calls","message":{"content":null,"tool_calls":[
            {"id":"a","function":{"name":"one","arguments":"{}"}},
            {"id":"b","function":{"name":"two","arguments":"{\"x\":2}"}}
        ]}}]})).unwrap();
        let body = chat_request("m", "", &[], &[
            json!({"role":"assistant","content":response["content"]}),
            json!({"role":"user","content":[
                {"type":"tool_result","tool_use_id":"a","content":"Failed","is_error":true},
                {"type":"tool_result","tool_use_id":"b","content":"Done"}
            ]})
        ]).unwrap();
        assert!(body["messages"][0]["content"].is_null());
        assert_eq!(body["messages"][0]["tool_calls"].as_array().unwrap().len(), 2);
        assert_eq!(body["messages"][1]["tool_call_id"], "a");
        assert_eq!(body["messages"][2]["tool_call_id"], "b");
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn recorded_text_and_length_responses() {
        for (reason, expected) in [("stop", "end_turn"), ("length", "max_tokens")] {
            let r = chat_response(&json!({"choices":[{"finish_reason":reason,"message":{"content":"Hello"}}]})).unwrap();
            assert_eq!(r, json!({"content":[{"type":"text","text":"Hello"}],"stop_reason":expected}));
        }
        assert!(chat_response(&json!({"choices":[]})).is_err());
        assert!(chat_response(&json!({"choices":[{"message":{"tool_calls":[{"function":{"arguments":"broken"}}]}}]})).is_err());
    }
}
