//! Realtime voice adapters: Alibaba DashScope, and the OpenAI Realtime protocol
//! (OpenAI, Azure OpenAI, xAI Grok, or any compatible server). Input is 16 kHz microphone
//! audio; events expose 24 kHz PCM16 for the existing player.

use std::time::Duration;

use anyhow::{Context, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// 设置了自己的大模型时，实时语音模型只负责听和说：判断、调工具都交给那个大模型，
/// 想好的回复再交给它读出来
pub const VOICE_PROMPT: &str = "你是语音播报员，只负责把文字读出来。用户消息里“【朗读】”后面的文字，用自然、口语的语气原样读出来，一字不改，不要回答、解释或补充任何内容。没有【朗读】标记的内容一律不要回应。";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider { DashScope, OpenAi, Azure, Xai, Custom }

/// Ids in the order shown in Settings
pub const IDS: [&str; 5] = ["dashscope", "openai", "azure", "xai", "custom"];

impl Provider {
    pub fn from_id(id: &str) -> Self {
        match id {
            "openai" => Self::OpenAi,
            "azure" => Self::Azure,
            "xai" => Self::Xai,
            "custom" => Self::Custom,
            _ => Self::DashScope,
        }
    }

    /// Speaks the OpenAI Realtime protocol (24 kHz audio, GA event names)
    fn openai_like(self) -> bool {
        self != Self::DashScope
    }

    /// (url, model, voice) defaults when the provider is picked in Settings
    pub fn defaults(id: &str) -> (&'static str, &'static str, &'static str) {
        match id {
            "openai" => ("wss://api.openai.com/v1/realtime", "gpt-realtime-2", "marin"),
            "azure" => ("wss://YOUR-RESOURCE.openai.azure.com/openai/v1/realtime", "gpt-realtime", "marin"),
            "xai" => ("wss://api.x.ai/v1/realtime", "grok-voice-latest", "eve"),
            "custom" => ("ws://127.0.0.1:8000/v1/realtime", "", "alloy"),
            _ => ("wss://dashscope.aliyuncs.com/api-ws/v1/realtime", "qwen-audio-3.1-realtime-plus", "longanqian_v3.1"),
        }
    }
}

pub struct RealtimeSettings {
    pub provider: Provider,
    pub listen_only: bool,
    pub url: String,
    pub model: String,
    pub voice: String,
    pub api_key: String,
    pub instructions: String,
    /// Anthropic 格式的工具定义，连接时转换成这里要的格式
    pub tools: Vec<Value>,
    pub speech_rate: f32,
}

pub enum Event {
    /// 用户开始说话（服务端会自动打断正在进行的回复）
    SpeechStarted,
    SpeechStopped,
    /// 说话结束但服务端判定不算一句话（turn_invalid）：不会有回复，只需要结束“正在说”
    SpeechInvalid,
    UserPartial(String),
    UserText(String),
    /// 助手回复的文字（随语音一起流式返回）
    Transcript(String),
    /// 24k PCM16 语音
    Audio(Vec<u8>),
    FunctionCall { call_id: String, name: String, arguments: String },
    /// 服务端开始生成一轮回复（包括说完话后它自动开始的那种）
    ResponseCreated,
    /// 一轮回复结束：completed / cancelled / failed
    ResponseDone(String),
    /// 服务端报告的请求错误（连接还在）；已知无害的（比如没有回复时发了取消）不会报上来
    Error(String),
    Other,
}

pub(crate) type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub struct Session {
    ws: Ws,
    provider: Provider,
    input_resampler: InputResampler,
    /// Listen-only on a server that always answers by itself (xAI): replies we did not ask for are cancelled and dropped
    guard_replies: bool,
    asked: bool,
    dropping: bool,
}

/// 连接、握手、等配置确认整体的截止时间
pub const CONNECT_DEADLINE: Duration = Duration::from_secs(15);

impl Session {
    /// 连接并完成配置；整体不超过 CONNECT_DEADLINE
    pub async fn connect(s: &RealtimeSettings) -> anyhow::Result<Self> {
        Self::connect_within(s, CONNECT_DEADLINE).await
    }

    pub async fn connect_within(s: &RealtimeSettings, deadline: Duration) -> anyhow::Result<Self> {
        match tokio::time::timeout(deadline, Self::connect_inner(s)).await {
            Ok(r) => r,
            Err(_) => bail!("连接实时语音服务超时（{} 秒内没有完成配置）", deadline.as_secs()),
        }
    }

    async fn connect_inner(s: &RealtimeSettings) -> anyhow::Result<Self> {
        if s.url.contains("/api-ws/v1/inference") {
            bail!("{}", crate::locale::pick(
                "This address is Alibaba speech recognition, not a realtime voice service. Switch the mode to Standard, pick Alibaba DashScope under Speech recognition, and enter this address and model there.",
                "这个地址是阿里的语音识别服务，不是实时语音。请把对话方式换成“标准”，语音识别选“阿里云百炼”，在那里填这个地址和模型。",
            ));
        }
        let url = session_url(&s.url, &s.model);
        let mut request = url.as_str().into_client_request()?;
        if s.provider == Provider::Azure {
            request.headers_mut().insert("api-key", s.api_key.parse()?);
        } else if !s.api_key.is_empty() {
            request.headers_mut().insert("Authorization", format!("Bearer {}", s.api_key).parse()?);
        }
        let (ws, _) = tokio::time::timeout(Duration::from_secs(8), connect_async(request))
            .await
            .context("连接实时语音服务超时")?
            .context("连接实时语音服务失败（检查 API Key、模型是否已开通和网络）")?;
        let guard_replies = s.listen_only && s.provider == Provider::Xai;
        let mut session = Self { ws, provider: s.provider, input_resampler: InputResampler::default(), guard_replies, asked: false, dropping: false };
        let tools: Vec<Value> = s
            .tools
            .iter()
            .map(|t| json!({"type": "function", "function": {"name": t["name"], "description": t["description"], "parameters": t["input_schema"]}}))
            .collect();
        let mut config = json!({
            "modalities": ["audio", "text"],
            "voice": s.voice,
            "speech_rate": s.speech_rate,
            "instructions": s.instructions,
            "input_audio_format": "pcm",
            "output_audio_format": "pcm",
            "input_audio_transcription": {"language": "zh"},
            "turn_detection": {"type": "smart_turn_v2"},
        });
        if s.provider == Provider::Xai {
            // Close to OpenAI's protocol, but voice and turn detection sit at the top of the session
            config = json!({
                "instructions": s.instructions,
                "voice": s.voice,
                "turn_detection": {"type": "server_vad"},
                "audio": {
                    "input": {
                        "format": {"type": "audio/pcm", "rate": 24000},
                        "transcription": {"model": "grok-transcribe", "language_hint": crate::locale::pick("en", "zh")}
                    },
                    "output": {"format": {"type": "audio/pcm", "rate": 24000}, "speed": s.speech_rate.clamp(0.5, 1.5)}
                }
            });
            if !s.tools.is_empty() {
                config["tools"] = json!(s.tools.iter().map(|t| json!({"type":"function", "name": t["name"], "description": t["description"], "parameters": t["input_schema"]})).collect::<Vec<_>>());
            }
        } else if s.provider.openai_like() {
            config = json!({
                "type": "realtime",
                "output_modalities": ["audio"],
                "instructions": s.instructions,
                "audio": {
                    "input": {
                        "format": {"type": "audio/pcm", "rate": 24000},
                        "transcription": {"model": "gpt-transcribe", "language": crate::locale::pick("en", "zh")},
                        "turn_detection": {"type": "server_vad", "create_response": !s.listen_only, "interrupt_response": true}
                    },
                    "output": {"format": {"type": "audio/pcm", "rate": 24000}, "voice": s.voice, "speed": s.speech_rate.clamp(0.25, 1.5)}
                }
            });
            if !s.tools.is_empty() {
                config["tools"] = json!(s.tools.iter().map(|t| json!({"type":"function", "name": t["name"], "description": t["description"], "parameters": t["input_schema"]})).collect::<Vec<_>>());
            }
        } else if !tools.is_empty() {
            config["tools"] = json!(tools);
        }
        session.send(json!({"type": "session.update", "session": config})).await?;
        // 等 session.updated；参数有错时服务端回 error
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(8), session.ws.next()).await.context("实时语音服务无响应")?;
            let v = parse(msg)?;
            match v["type"].as_str() {
                Some("session.updated") => return Ok(session),
                Some("error") => bail!("实时语音配置失败：{}", v["error"]["message"].as_str().unwrap_or("未知错误")),
                _ => {}
            }
        }
    }

    async fn send(&mut self, v: Value) -> anyhow::Result<()> {
        self.ws.send(Message::text(v.to_string())).await.context("发送到实时语音服务失败")
    }

    /// 发送 16k 单声道音频
    pub async fn send_audio(&mut self, samples: &[f32]) -> anyhow::Result<()> {
        let converted;
        let samples = if self.provider.openai_like() {
            converted = self.input_resampler.convert(samples);
            converted.as_slice()
        } else { samples };
        let mut pcm = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            pcm.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
        }
        self.send(json!({"type": "input_audio_buffer.append", "audio": base64_encode(&pcm)})).await
    }

    pub async fn send_function_output(&mut self, call_id: &str, output: &str) -> anyhow::Result<()> {
        // output 要求是 JSON 字符串
        let output = json!({"result": output}).to_string();
        self.send(json!({"type": "conversation.item.create", "item": {"type": "function_call_output", "call_id": call_id, "output": output}}))
            .await?;
        self.create_response().await
    }

    /// 插入一条文字消息（打字输入、后台任务完成通知）并让模型回复
    pub async fn say(&mut self, text: &str) -> anyhow::Result<()> {
        self.send(json!({"type": "conversation.item.create", "item": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]}}))
            .await?;
        self.create_response().await
    }

    /// 只当嘴巴用时：把想好的回复交给它原样读出来（配合 VOICE_PROMPT）
    pub async fn read_aloud(&mut self, text: &str) -> anyhow::Result<()> {
        self.say(&format!("【朗读】{text}")).await
    }

    pub async fn create_response(&mut self) -> anyhow::Result<()> {
        self.asked = true;
        self.send(json!({"type": "response.create"})).await
    }

    /// 尽力取消当前回复；最多等 1 秒，连接半开时也不会挂住调用方
    pub async fn cancel_response(&mut self) {
        let _ = tokio::time::timeout(Duration::from_secs(1), self.send(json!({"type": "response.cancel"}))).await;
    }

    pub async fn close(mut self) {
        let _ = self.ws.close(None).await;
    }

    /// 下一个事件；连接断开时返回错误
    pub async fn next_event(&mut self) -> anyhow::Result<Event> {
        let v = parse(self.ws.next().await)?;
        let kind = v["type"].as_str().unwrap_or("");
        if self.guard_replies {
            match kind {
                "response.created" if !self.asked => {
                    self.dropping = true;
                    self.cancel_response().await;
                    return Ok(Event::Other);
                }
                "response.created" => self.asked = false,
                "response.done" if self.dropping => {
                    self.dropping = false;
                    return Ok(Event::Other);
                }
                _ if self.dropping && kind.starts_with("response.") => return Ok(Event::Other),
                _ => {}
            }
        }
        let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
        Ok(match kind {
            "input_audio_buffer.speech_started" => Event::SpeechStarted,
            "input_audio_buffer.speech_stopped" if v["reason"] == "turn_invalid" => Event::SpeechInvalid,
            "input_audio_buffer.speech_stopped" => Event::SpeechStopped,
            "conversation.item.input_audio_transcription.delta" => Event::UserPartial(if self.provider.openai_like() { s("delta") } else { format!("{}{}", s("text"), s("stash")) }),
            // xAI: the whole transcript so far
            "conversation.item.input_audio_transcription.updated" => Event::UserPartial(s("transcript")),
            "conversation.item.input_audio_transcription.completed" => Event::UserText(s("transcript")),
            "response.output_audio_transcript.delta" | "response.output_text.delta" | "response.audio_transcript.delta" | "response.text.delta" => Event::Transcript(s("delta")),
            "response.output_audio.delta" | "response.audio.delta" => Event::Audio(base64_decode(v["delta"].as_str().unwrap_or(""))),
            "response.function_call_arguments.done" => {
                Event::FunctionCall { call_id: s("call_id"), name: s("name"), arguments: s("arguments") }
            }
            "response.created" => Event::ResponseCreated,
            "response.done" => Event::ResponseDone(v["response"]["status"].as_str().unwrap_or("completed").to_string()),
            "error" => {
                let err = &v["error"];
                let message = err["message"].as_str().unwrap_or("未知错误");
                if err["type"] == "server_error" {
                    bail!("实时语音服务出错：{message}");
                }
                eprintln!("实时语音请求错误：{message}"); // 客户端错误不断开连接
                if is_benign_error(err) { Event::Other } else { Event::Error(message.to_string()) }
            }
            _ => Event::Other,
        })
    }
}

// Continuous linear interpolation from the microphone's 16 kHz to OpenAI's
// 24 kHz. Retain the boundary sample and fractional position across chunks.
#[derive(Default)]
struct InputResampler {
    pending: Vec<f32>,
    position: f64,
}

impl InputResampler {
    fn convert(&mut self, samples: &[f32]) -> Vec<f32> {
        self.pending.extend_from_slice(samples);
        let mut out = Vec::with_capacity(samples.len() * 3 / 2 + 2);
        while self.position + 1.0 < self.pending.len() as f64 {
            let i = self.position.floor() as usize;
            let t = (self.position - i as f64) as f32;
            out.push(self.pending[i] * (1.0 - t) + self.pending[i + 1] * t);
            self.position += 2.0 / 3.0;
        }
        let consumed = self.position.floor() as usize;
        self.pending.drain(..consumed);
        self.position -= consumed as f64;
        out
    }
}

/// 拼连接地址；地址里没有路径时补上“/”，否则请求行不合法
fn session_url(base: &str, model: &str) -> String {
    let base = base.trim_end_matches('/');
    let has_path = base.split_once("://").is_some_and(|(_, rest)| rest.contains('/'));
    if has_path { format!("{base}?model={model}") } else { format!("{base}/?model={model}") }
}

/// 白名单：只有这些错误静默——我们主动取消时服务端恰好已经没有进行中的回复
fn is_benign_error(err: &Value) -> bool {
    let code = err["code"].as_str().unwrap_or("").to_lowercase();
    let message = err["message"].as_str().unwrap_or("").to_lowercase();
    code == "response_cancel_not_active"
        || message.contains("no active response")
        || message.contains("none active response")
        || message.contains("cancellation failed")
}

fn parse(msg: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>) -> anyhow::Result<Value> {
    match msg {
        Some(Ok(Message::Text(t))) => Ok(serde_json::from_str(t.as_str())?),
        Some(Ok(Message::Close(frame))) => bail!("实时语音连接被关闭：{}", frame.map(|f| f.reason.to_string()).unwrap_or_default()),
        Some(Ok(_)) => Ok(Value::Null),
        Some(Err(e)) => Err(e.into()),
        None => bail!("实时语音连接已断开"),
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub(crate) fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn base64_decode(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0);
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue, // '=' 和换行
        };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio::net::TcpListener;

    fn settings(url: String) -> RealtimeSettings {
        RealtimeSettings {
            provider: Provider::DashScope,
            listen_only: false,
            url,
            model: "m".into(),
            voice: "v".into(),
            api_key: "test".into(),
            instructions: String::new(),
            tools: Vec::new(),
            speech_rate: 1.0,
        }
    }

    /// 本地 WebSocket 替身：收到 session.update 后按 script 依次回消息，然后一直挂着不关
    async fn fake_server(script: Vec<Value>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let _ = ws.next().await; // session.update
            for v in script {
                ws.send(Message::text(v.to_string())).await.unwrap();
            }
            while ws.next().await.is_some() {}
        });
        format!("ws://{addr}/realtime")
    }

    #[tokio::test]
    async fn openai_session_schema_audio_events_and_interrupt() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let update = parse(ws.next().await).unwrap();
            assert_eq!(update["session"]["type"], "realtime");
            assert_eq!(update["session"]["audio"]["input"]["format"]["rate"], 24000);
            assert_eq!(update["session"]["audio"]["output"]["format"]["rate"], 24000);
            assert_eq!(update["session"]["audio"]["input"]["turn_detection"]["create_response"], false);
            assert_eq!(update["session"]["tools"][0]["name"], "weather");
            ws.send(Message::text(json!({"type":"session.updated"}).to_string())).await.unwrap();
            let audio = parse(ws.next().await).unwrap();
            assert_eq!(audio["type"], "input_audio_buffer.append");
            assert!((base64_decode(audio["audio"].as_str().unwrap()).len() as i32 - 4800).abs() <= 4);
            assert_eq!(parse(ws.next().await).unwrap()["type"], "response.cancel");
            for event in [
                json!({"type":"conversation.item.input_audio_transcription.completed","transcript":"Hello"}),
                json!({"type":"response.output_audio_transcript.delta","delta":"Hi"}),
                json!({"type":"response.output_audio.delta","delta":"AQIDBA=="}),
                json!({"type":"response.done","response":{"status":"cancelled"}}),
            ] { ws.send(Message::text(event.to_string())).await.unwrap(); }
            while ws.next().await.is_some() {}
        });
        let mut cfg = settings(format!("ws://{addr}/realtime"));
        cfg.provider = Provider::OpenAi;
        cfg.listen_only = true;
        cfg.tools = vec![json!({"name":"weather","description":"Weather","input_schema":{"type":"object"}})];
        let mut session = Session::connect(&cfg).await.unwrap();
        session.send_audio(&vec![0.1; 1600]).await.unwrap();
        session.cancel_response().await;
        assert!(matches!(session.next_event().await.unwrap(), Event::UserText(t) if t == "Hello"));
        assert!(matches!(session.next_event().await.unwrap(), Event::Transcript(t) if t == "Hi"));
        assert!(matches!(session.next_event().await.unwrap(), Event::Audio(b) if b == [1,2,3,4]));
        assert!(matches!(session.next_event().await.unwrap(), Event::ResponseDone(t) if t == "cancelled"));
        session.close().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn xai_listen_only_drops_replies_it_did_not_ask_for() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (tcp, req) = (listener.accept().await.unwrap().0, ());
            let _ = req;
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let update = parse(ws.next().await).unwrap();
            assert_eq!(update["session"]["voice"], "eve");
            assert_eq!(update["session"]["turn_detection"]["type"], "server_vad");
            assert_eq!(update["session"]["audio"]["input"]["transcription"]["model"], "grok-transcribe");
            ws.send(Message::text(json!({"type":"session.updated"}).to_string())).await.unwrap();
            // The server answers the user on its own: Aeri must cancel it and hide its audio
            for event in [
                json!({"type":"conversation.item.input_audio_transcription.updated","transcript":"Hel"}),
                json!({"type":"conversation.item.input_audio_transcription.completed","transcript":"Hello"}),
                json!({"type":"response.created"}),
            ] { ws.send(Message::text(event.to_string())).await.unwrap(); }
            assert_eq!(parse(ws.next().await).unwrap()["type"], "response.cancel");
            for event in [
                json!({"type":"response.output_audio.delta","delta":"AQIDBA=="}),
                json!({"type":"response.done","response":{"status":"cancelled"}}),
            ] { ws.send(Message::text(event.to_string())).await.unwrap(); }
            // Then Aeri asks for the reply it wants read aloud
            assert_eq!(parse(ws.next().await).unwrap()["type"], "conversation.item.create");
            assert_eq!(parse(ws.next().await).unwrap()["type"], "response.create");
            for event in [
                json!({"type":"response.created"}),
                json!({"type":"response.output_audio.delta","delta":"AQIDBA=="}),
                json!({"type":"response.done","response":{"status":"completed"}}),
            ] { ws.send(Message::text(event.to_string())).await.unwrap(); }
            while ws.next().await.is_some() {}
        });
        let mut cfg = settings(format!("ws://{addr}/v1/realtime"));
        cfg.provider = Provider::Xai;
        cfg.listen_only = true;
        cfg.voice = "eve".into();
        let mut session = Session::connect(&cfg).await.unwrap();
        assert!(matches!(session.next_event().await.unwrap(), Event::UserPartial(t) if t == "Hel"));
        assert!(matches!(session.next_event().await.unwrap(), Event::UserText(t) if t == "Hello"));
        assert!(matches!(session.next_event().await.unwrap(), Event::Other)); // own reply cancelled
        assert!(matches!(session.next_event().await.unwrap(), Event::Other)); // its audio dropped
        assert!(matches!(session.next_event().await.unwrap(), Event::Other)); // its done dropped
        session.read_aloud("Hi").await.unwrap();
        assert!(matches!(session.next_event().await.unwrap(), Event::ResponseCreated));
        assert!(matches!(session.next_event().await.unwrap(), Event::Audio(b) if b == [1,2,3,4]));
        assert!(matches!(session.next_event().await.unwrap(), Event::ResponseDone(t) if t == "completed"));
        session.close().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn azure_uses_api_key_header() {
        use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut seen = (String::new(), false);
            let mut ws = tokio_tungstenite::accept_hdr_async(tcp, |req: &Request, resp: Response| {
                seen = (req.headers().get("api-key").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default(), req.headers().contains_key("Authorization"));
                Ok(resp)
            }).await.unwrap();
            assert_eq!(seen, ("secret".to_string(), false));
            let update = parse(ws.next().await).unwrap();
            assert_eq!(update["session"]["type"], "realtime");
            ws.send(Message::text(json!({"type":"session.updated"}).to_string())).await.unwrap();
            while ws.next().await.is_some() {}
        });
        let mut cfg = settings(format!("ws://{addr}/openai/v1/realtime"));
        cfg.provider = Provider::Azure;
        cfg.api_key = "secret".into();
        let session = Session::connect(&cfg).await.unwrap();
        session.close().await;
        server.await.unwrap();
    }

    #[test]
    fn resampler_preserves_stream_boundaries() {
        let samples: Vec<f32> = (0..1600).map(|i| i as f32 / 1600.0).collect();
        let whole = InputResampler::default().convert(&samples);
        let mut resampler = InputResampler::default();
        let split: Vec<f32> = samples.chunks(37).flat_map(|c| resampler.convert(c)).collect();
        assert_eq!(whole.len(), split.len());
        assert!((whole.len() as i32 - 2400).abs() <= 2);
        for (a, b) in whole.iter().zip(split) { assert!((a-b).abs() < 0.00001); }
    }

    #[test]
    fn url_without_path_still_valid() {
        assert_eq!(session_url("ws://127.0.0.1:1", "m"), "ws://127.0.0.1:1/?model=m");
        assert_eq!(session_url("wss://h/api/realtime/", "m"), "wss://h/api/realtime?model=m");
    }

    #[tokio::test]
    async fn connect_has_overall_deadline_even_if_server_keeps_chatting() {
        // 服务端一直回无关消息、从不确认配置：单条消息的 8 秒超时永远触发不了，必须靠整体截止
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (got_update_tx, got_update_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            if matches!(ws.next().await, Some(Ok(Message::Text(t))) if t.as_str().contains("session.update")) {
                let _ = got_update_tx.send(());
            }
            loop {
                if ws.send(Message::text(json!({"type": "session.created"}).to_string())).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });
        let started = std::time::Instant::now();
        let r = Session::connect_within(&settings(format!("ws://{addr}/realtime")), Duration::from_millis(600)).await;
        let elapsed = started.elapsed();
        let err = r.err().expect("从不确认配置时必须失败");
        assert!(format!("{err:#}").contains("超时"), "{err:#}");
        // 确实握手成功、发了配置，然后一直等到整体截止才放弃
        assert!(got_update_rx.await.is_ok(), "服务端应收到 session.update");
        assert!(elapsed >= Duration::from_millis(550) && elapsed < Duration::from_secs(3), "{elapsed:?}");
    }

    #[tokio::test]
    async fn connect_reports_config_error() {
        let url = fake_server(vec![json!({"type": "error", "error": {"type": "invalid_request_error", "message": "voice not found"}})]).await;
        let err = Session::connect_within(&settings(url), Duration::from_secs(5)).await.err().unwrap();
        assert!(format!("{err:#}").contains("voice not found"));
    }

    #[tokio::test]
    async fn request_errors_surface_except_whitelisted() {
        let url = fake_server(vec![
            json!({"type": "session.updated"}),
            json!({"type": "error", "error": {"type": "invalid_request_error", "code": "response_cancel_not_active", "message": "Conversation has none active response"}}),
            json!({"type": "error", "error": {"type": "invalid_request_error", "message": "audio format invalid"}}),
            json!({"type": "error", "error": {"type": "server_error", "message": "boom"}}),
        ])
        .await;
        let mut session = Session::connect_within(&settings(url), Duration::from_secs(5)).await.unwrap();
        assert!(matches!(session.next_event().await.unwrap(), Event::Other));
        match session.next_event().await.unwrap() {
            Event::Error(m) => assert_eq!(m, "audio format invalid"),
            _ => panic!("非白名单错误应该报上来"),
        }
        assert!(session.next_event().await.is_err(), "server_error 应断开");
    }

    #[tokio::test]
    async fn invalid_turn_ends_speaking_instead_of_vanishing() {
        let url = fake_server(vec![
            json!({"type": "session.updated"}),
            json!({"type": "input_audio_buffer.speech_started"}),
            json!({"type": "input_audio_buffer.speech_stopped", "reason": "turn_invalid"}),
            json!({"type": "input_audio_buffer.speech_stopped"}),
        ])
        .await;
        let mut session = Session::connect_within(&settings(url), Duration::from_secs(5)).await.unwrap();
        assert!(matches!(session.next_event().await.unwrap(), Event::SpeechStarted));
        assert!(matches!(session.next_event().await.unwrap(), Event::SpeechInvalid));
        assert!(matches!(session.next_event().await.unwrap(), Event::SpeechStopped));
    }

    #[test]
    fn base64_roundtrip() {
        for data in [&b""[..], b"f", b"fo", b"foo", b"foobar", &[0, 255, 128, 7, 9]] {
            assert_eq!(base64_decode(&base64_encode(data)), data);
        }
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
    }
}
