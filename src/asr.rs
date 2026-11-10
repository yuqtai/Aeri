//! Streaming local and DashScope recognition, plus OpenAI-compatible
//! utterance transcription for OpenAI and Groq. All capture uses 16 kHz PCM.

use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider { Local, OpenAi, Groq, DashScope }

impl Provider {
    pub fn from_id(id: &str) -> Self {
        match id { "local" => Self::Local, "openai" => Self::OpenAi, "groq" => Self::Groq, _ => Self::DashScope }
    }
}

pub struct AsrSettings {
    pub provider: Provider,
    pub url: String,
    pub model: String,
    pub api_key: String,
    pub sentence_silence_ms: u32,
    /// 多久没开口就放弃
    pub speech_timeout: Duration,
}

const MAX_SESSION: Duration = Duration::from_secs(30);
const CHUNK_SAMPLES: usize = 1600; // 每 100ms 发一次

type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Message,
>;
type WsStream = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

/// 测试 key 和网络：建立连接、启动任务、立即结束
pub async fn check(s: &AsrSettings) -> anyhow::Result<()> {
    match s.provider {
        Provider::Local => {
            let dir = prepare_local(|_| {}).await?;
            return tokio::task::spawn_blocking(move || local_recognizer(&dir).map(|_| ())).await?;
        }
        Provider::OpenAi | Provider::Groq => return transcribe(s, &vec![0.0; 16000]).await.map(|_| ()),
        Provider::DashScope => {}
    }
    let (mut tx, mut rx, finish_task) = open_task(s).await?;
    tx.send(finish_task).await?;
    let _ = tokio::time::timeout(Duration::from_secs(3), rx.next()).await;
    let _ = tx.close().await;
    Ok(())
}

/// 连接服务并发送 run-task，等到 task-started 后返回（发送端，接收端，finish-task 消息）
async fn open_task(s: &AsrSettings) -> anyhow::Result<(WsSink, WsStream, Message)> {
    let mut request = s.url.as_str().into_client_request()?;
    request.headers_mut().insert("Authorization", format!("Bearer {}", s.api_key).parse()?);
    let (ws, _) = tokio::time::timeout(Duration::from_secs(8), connect_async(request))
        .await
        .context("连接语音识别服务超时")?
        .context("连接语音识别服务失败（检查 API Key、服务地址和网络）")?;
    let (mut tx, mut rx) = ws.split();

    let task_id = uuid::Uuid::new_v4().simple().to_string();
    let run_task = json!({
        "header": {"action": "run-task", "task_id": task_id, "streaming": "duplex"},
        "payload": {
            "task_group": "audio", "task": "asr", "function": "recognition", "model": s.model,
            "parameters": {
                "format": "pcm", "sample_rate": 16000, "language_hints": [crate::locale::pick("en", "zh")],
                "max_sentence_silence": s.sentence_silence_ms,
            },
            "input": {}
        }
    });
    tx.send(Message::text(run_task.to_string())).await?;
    let finish_task = Message::text(
        json!({"header": {"action": "finish-task", "task_id": task_id, "streaming": "duplex"}, "payload": {"input": {}}})
            .to_string(),
    );

    // 等 task-started；这期间麦克风音频在通道里排队，不会丢
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), rx.next()).await.context("语音识别服务无响应")?;
        let event = parse_event(msg)?;
        match event["header"]["event"].as_str() {
            Some("task-started") => break,
            Some("task-failed") => bail!("语音识别失败：{}", event["header"]["error_message"].as_str().unwrap_or("未知错误")),
            _ => {}
        }
    }
    Ok((tx, rx, finish_task))
}

/// 识别一句话。没人说话或没识别出内容时返回 Ok(None)。
pub async fn recognize(
    s: &AsrSettings,
    audio: &mut UnboundedReceiver<Vec<f32>>,
    mut on_partial: impl FnMut(&str),
) -> anyhow::Result<Option<String>> {
    match s.provider {
        Provider::Local => return recognize_local(s, audio, on_partial).await,
        Provider::OpenAi | Provider::Groq => {
            let samples = collect_utterance(s, audio).await;
            if samples.is_empty() { return Ok(None); }
            let text = transcribe(s, &samples).await?;
            on_partial(&text);
            return Ok((!text.is_empty()).then_some(text));
        }
        Provider::DashScope => {}
    }
    let (mut tx, mut rx, finish_task) = open_task(s).await?;
    let started = Instant::now();
    let mut pcm: Vec<u8> = Vec::with_capacity(CHUNK_SAMPLES * 2);
    let mut heard = false;
    let mut finished_at: Option<Instant> = None;
    let mut text = String::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(100));

    loop {
        tokio::select! {
            chunk = audio.recv(), if finished_at.is_none() => {
                let Some(chunk) = chunk else { break };
                for s in chunk {
                    pcm.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
                }
                if pcm.len() >= CHUNK_SAMPLES * 2 {
                    tx.send(Message::binary(std::mem::take(&mut pcm))).await?;
                }
            }
            msg = rx.next() => {
                let Some(msg) = msg else { break };
                let event = parse_event(Some(msg))?;
                match event["header"]["event"].as_str() {
                    Some("result-generated") => {
                        let sentence = &event["payload"]["output"]["sentence"];
                        if sentence["heartbeat"].as_bool() == Some(true) {
                            continue;
                        }
                        let partial = sentence["text"].as_str().unwrap_or("");
                        if !partial.is_empty() {
                            heard = true;
                            on_partial(partial);
                        }
                        if sentence["sentence_end"].as_bool() == Some(true) {
                            text.push_str(partial);
                            if finished_at.is_none() {
                                tx.send(finish_task.clone()).await?;
                                finished_at = Some(Instant::now());
                            }
                        }
                    }
                    Some("task-finished") => break,
                    Some("task-failed") => bail!("语音识别失败：{}", event["header"]["error_message"].as_str().unwrap_or("未知错误")),
                    _ => {}
                }
            }
            _ = ticker.tick() => {
                let timed_out = (!heard && started.elapsed() > s.speech_timeout) || started.elapsed() > MAX_SESSION;
                if finished_at.is_none() && timed_out {
                    if !pcm.is_empty() {
                        tx.send(Message::binary(std::mem::take(&mut pcm))).await?;
                    }
                    tx.send(finish_task.clone()).await?;
                    finished_at = Some(Instant::now());
                }
                if finished_at.is_some_and(|t| t.elapsed() > Duration::from_secs(3)) {
                    break;
                }
            }
        }
    }
    let _ = tx.close().await;
    let text = text.trim().to_string();
    Ok((!text.is_empty()).then_some(text))
}

fn parse_event(msg: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>) -> anyhow::Result<Value> {
    match msg {
        Some(Ok(Message::Text(t))) => Ok(serde_json::from_str(t.as_str())?),
        Some(Ok(Message::Close(frame))) => bail!("语音识别连接被关闭：{}", frame.map(|f| f.reason.to_string()).unwrap_or_default()),
        Some(Ok(_)) => Ok(Value::Null),
        Some(Err(e)) => Err(e.into()),
        None => bail!("语音识别连接已断开"),
    }
}

struct LocalModel {
    name: &'static str,
    sha256: &'static str,
    files: &'static [&'static str],
}

fn local_model(zh: bool) -> LocalModel {
    if zh {
        LocalModel {
            name: "sherpa-onnx-streaming-zipformer-small-ctc-zh-int8-2025-04-01",
            sha256: "b3b309f7ce4a737195fcc6963ea19b0653a7d3401580af5ae0d3e284cbb71f0b",
            files: &["model.int8.onnx", "tokens.txt"],
        }
    } else {
        LocalModel {
            name: "sherpa-onnx-streaming-zipformer-en-20M-2023-02-17",
            sha256: "9c559283e8498d3fe95913c79ca1cb454bb26281ac2b102b41306c7d752765d9",
            files: &["encoder-epoch-99-avg-1.int8.onnx", "decoder-epoch-99-avg-1.int8.onnx", "joiner-epoch-99-avg-1.int8.onnx", "tokens.txt"],
        }
    }
}

// Only the pinned official archive is extracted, and only the required files.
// Nothing is shipped in the app zip or written beside the executable.
pub async fn prepare_local(mut status: impl FnMut(&str)) -> anyhow::Result<std::path::PathBuf> {
    use std::io::Write;
    use sha2::{Digest, Sha256};
    use crate::config::NoWindow;
    static INSTALL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _install = INSTALL.lock().await;
    let model = local_model(crate::locale::is_zh());
    let root = crate::config::app_dir().join("models");
    let dir = root.join(model.name);
    if model.files.iter().all(|f| dir.join(f).is_file()) { return Ok(dir); }
    status(crate::locale::pick("Downloading the local speech model for first use. This may take a few minutes…", "首次使用：正在下载本地语音识别模型，可能需要几分钟…"));
    std::fs::create_dir_all(&root)?;
    let path = root.join(format!("{}.tar.bz2", uuid::Uuid::new_v4()));
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup { fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); } }
    let _cleanup = Cleanup(path.clone());
    let mut file = std::fs::File::create(&path)?;
    let url = format!("https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/{}.tar.bz2", model.name);
    let mut response = reqwest::Client::builder().timeout(Duration::from_secs(600)).build()?.get(url).send().await?.error_for_status()?;
    let mut hash = Sha256::new();
    while let Some(chunk) = response.chunk().await? { hash.update(&chunk); file.write_all(&chunk)?; }
    drop(file);
    if format!("{:x}", hash.finalize()) != model.sha256 { bail!("Local speech model SHA-256 verification failed; retry the download."); }
    status(crate::locale::pick("Verified the download. Preparing local speech recognition…", "下载校验通过，正在准备本地语音识别…"));
    let mut command = tokio::process::Command::new("tar.exe");
    let staging = root.join(format!("install-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&staging)?;
    command.creation_flags_no_window();
    command.kill_on_drop(true);
    command.arg("-xjf").arg(&path).arg("-C").arg(&staging);
    for file in model.files {
        let prefix = if crate::locale::is_zh() { "./" } else { "" };
        command.arg(format!("{prefix}{}/{file}", model.name));
    }
    let result = command.output().await.context("Extracting the local speech model requires Windows tar.exe")?;
    let extracted = staging.join(model.name);
    if !result.status.success() || !model.files.iter().all(|f| extracted.join(f).is_file()) { bail!("Could not extract the local speech model; retry or select a cloud provider."); }
    std::fs::rename(&extracted, &dir).context("Installing the local speech model failed")?;
    let _ = std::fs::remove_dir(&staging);
    Ok(dir)
}

fn local_recognizer(dir: &std::path::Path) -> anyhow::Result<sherpa_onnx::OnlineRecognizer> {
    let mut c = sherpa_onnx::OnlineRecognizerConfig::default();
    let file = |f: &str| Some(dir.join(f).to_string_lossy().into_owned());
    if dir.join("model.int8.onnx").exists() {
        c.model_config.zipformer2_ctc.model = file("model.int8.onnx");
    } else {
        c.model_config.transducer.encoder = file("encoder-epoch-99-avg-1.int8.onnx");
        c.model_config.transducer.decoder = file("decoder-epoch-99-avg-1.int8.onnx");
        c.model_config.transducer.joiner = file("joiner-epoch-99-avg-1.int8.onnx");
    }
    c.model_config.tokens = file("tokens.txt");
    c.model_config.num_threads = 2;
    c.decoding_method = Some("greedy_search".into());
    c.enable_endpoint = true;
    c.rule1_min_trailing_silence = 2.4;
    c.rule2_min_trailing_silence = 0.8;
    c.rule3_min_utterance_length = 30.0;
    sherpa_onnx::OnlineRecognizer::create(&c).context("Could not load the local speech model")
}

async fn recognize_local(s: &AsrSettings, audio: &mut UnboundedReceiver<Vec<f32>>, mut partial: impl FnMut(&str)) -> anyhow::Result<Option<String>> {
    let dir = prepare_local(|text| partial(text)).await?;
    let recognizer = tokio::task::spawn_blocking(move || local_recognizer(&dir)).await??;
    let stream = recognizer.create_stream();
    let started = Instant::now();
    let deadline = tokio::time::sleep(MAX_SESSION);
    tokio::pin!(deadline);
    let mut text = String::new();
    loop {
        let chunk = tokio::select! {
            chunk = audio.recv() => match chunk { Some(c) => c, None => break },
            _ = &mut deadline => break,
            _ = tokio::time::sleep(s.speech_timeout), if text.is_empty() => break,
        };
        stream.accept_waveform(16000, &chunk);
        while recognizer.is_ready(&stream) { recognizer.decode(&stream); }
        if let Some(result) = recognizer.get_result(&stream) {
            if result.text != text { text = result.text; partial(&text); }
        }
        if recognizer.is_endpoint(&stream) && !text.is_empty() { break; }
        if text.is_empty() && started.elapsed() > s.speech_timeout { break; }
    }
    stream.accept_waveform(16000, &vec![0.0; 4800]);
    stream.input_finished();
    while recognizer.is_ready(&stream) { recognizer.decode(&stream); }
    if let Some(result) = recognizer.get_result(&stream) { text = result.text; }
    let text = text.trim().to_string();
    Ok((!text.is_empty()).then_some(text))
}

// Cloud transcription endpoints accept complete WAV utterances. The capture
// boundary remains streaming, with a conservative energy gate for end of speech.
async fn collect_utterance(s: &AsrSettings, audio: &mut UnboundedReceiver<Vec<f32>>) -> Vec<f32> {
    let started = Instant::now();
    let mut samples = Vec::new();
    let mut heard = false;
    let mut silent_samples = 0usize;
    loop {
        let chunk = match tokio::time::timeout(Duration::from_millis(250), audio.recv()).await {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(_) => { if started.elapsed() > s.speech_timeout && !heard || started.elapsed() > MAX_SESSION { break; } continue; }
        };
        let energy = chunk.iter().map(|s| s*s).sum::<f32>() / chunk.len().max(1) as f32;
        if energy > 0.0001 { heard = true; silent_samples = 0; } else if heard { silent_samples += chunk.len(); }
        samples.extend(chunk);
        if silent_samples >= s.sentence_silence_ms as usize * 16 || started.elapsed() > MAX_SESSION || (!heard && started.elapsed() > s.speech_timeout) { break; }
    }
    if heard { samples } else { Vec::new() }
}

fn wav(samples: &[f32]) -> Vec<u8> {
    let len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(len as usize + 44);
    out.extend_from_slice(b"RIFF"); out.extend_from_slice(&(len + 36).to_le_bytes()); out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); out.extend_from_slice(&1u16.to_le_bytes()); out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&16000u32.to_le_bytes()); out.extend_from_slice(&32000u32.to_le_bytes()); out.extend_from_slice(&2u16.to_le_bytes()); out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data"); out.extend_from_slice(&len.to_le_bytes());
    for s in samples { out.extend_from_slice(&((s.clamp(-1.0, 1.0)*32767.0) as i16).to_le_bytes()); }
    out
}

async fn transcribe(s: &AsrSettings, samples: &[f32]) -> anyhow::Result<String> {
    let boundary = format!("aeri{}", uuid::Uuid::new_v4().simple());
    let mut body = Vec::new();
    for (name, value) in [("model", s.model.as_str()), ("language", crate::locale::pick("en", "zh"))] {
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
    }
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"speech.wav\"\r\nContent-Type: audio/wav\r\n\r\n").as_bytes());
    body.extend(wav(samples)); body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let base = s.url.trim_end_matches('/');
    let url = if base.ends_with("/audio/transcriptions") { base.to_string() } else { format!("{base}/audio/transcriptions") };
    let response = reqwest::Client::builder().timeout(Duration::from_secs(60)).build()?.post(url).bearer_auth(&s.api_key)
        .header("Content-Type", format!("multipart/form-data; boundary={boundary}")).body(body).send().await.context("Speech transcription request failed")?;
    if !response.status().is_success() { bail!("Speech transcription failed (HTTP {}); check the provider, key, and model.", response.status()); }
    let result: Value = response.json().await?;
    Ok(result["text"].as_str().context("Missing transcription text")?.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn openai_compatible_transcription_posts_wav_multipart() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let body_start;
            let content_len;
            loop {
                let mut bytes = [0; 4096];
                let n = socket.read(&mut bytes).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&bytes[..n]);
                if let Some(i) = request.windows(4).position(|b| b == b"\r\n\r\n") {
                    body_start = i+4;
                    let headers = String::from_utf8_lossy(&request[..i]).to_ascii_lowercase();
                    assert!(headers.starts_with("post /v1/audio/transcriptions "));
                    assert!(headers.contains("multipart/form-data; boundary=aeri"));
                    content_len = headers.lines().find_map(|line| line.strip_prefix("content-length: ")).unwrap().parse::<usize>().unwrap();
                    break;
                }
            }
            while request.len() < body_start + content_len {
                let mut bytes = [0; 4096];
                let n = socket.read(&mut bytes).await.unwrap(); assert!(n > 0);
                request.extend_from_slice(&bytes[..n]);
            }
            let body = &request[body_start..];
            assert!(body.windows(4).any(|b| b == b"RIFF"));
            assert!(body.windows(b"filename=\"speech.wav\"".len()).any(|b| b == b"filename=\"speech.wav\""));
            let response = r#"{"text":"Hello from the recorded sample"}"#;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
        });
        let settings = AsrSettings { provider: Provider::OpenAi, url: format!("http://{addr}/v1"), model: "sample-model".into(), api_key: "sample-key".into(), sentence_silence_ms: 800, speech_timeout: Duration::from_secs(6) };
        assert_eq!(transcribe(&settings, &[0.0; 1600]).await.unwrap(), "Hello from the recorded sample");
        server.await.unwrap();
    }

    #[test]
    fn official_local_model_samples_when_available() {
        let Ok(root) = std::env::var("AERI_LOCAL_MODEL_TEST_ROOT") else { return };
        for zh in [false, true] {
            let model = local_model(zh);
            let dir = std::path::Path::new(&root).join(model.name);
            let recognizer = local_recognizer(&dir).unwrap();
            let wave = sherpa_onnx::Wave::read(dir.join("test_wavs/0.wav").to_str().unwrap()).unwrap();
            let stream = recognizer.create_stream();
            for chunk in wave.samples().chunks(1600) {
                stream.accept_waveform(wave.sample_rate(), chunk);
                while recognizer.is_ready(&stream) { recognizer.decode(&stream); }
            }
            stream.accept_waveform(16000, &vec![0.0; 4800]);
            stream.input_finished();
            while recognizer.is_ready(&stream) { recognizer.decode(&stream); }
            let text = recognizer.get_result(&stream).unwrap().text;
            assert!(!text.trim().is_empty(), "Official language sample must produce text (zh={zh})");
        }
    }

    #[test]
    fn wav_header_matches_pcm_payload() {
        let bytes = wav(&[0.0, 1.0, -1.0]);
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(bytes.len(), 50);
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 16000);
        assert_eq!(&bytes[44..], &[0, 0, 255, 127, 1, 128]);
    }
}
