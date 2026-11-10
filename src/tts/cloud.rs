//! Streaming raw PCM adapters for the cloud speech services.
use anyhow::{Context, ensure, bail};
use serde_json::json;
use crate::config::{self, Config};

pub fn defaults(provider: &str, zh: bool) -> (&'static str, &'static str) {
    match provider {
        "openai" => ("gpt-realtime-2.1-mini", "marin"),
        "elevenlabs" => ("eleven_flash_v2_5", "JBFqnCBsd6RMkjVDRZzb"),
        "azure" => ("", if zh { "zh-CN-XiaoxiaoNeural" } else { "en-US-JennyNeural" }),
        _ => ("", ""),
    }
}

pub(super) fn openai_settings(cfg: &Config) -> anyhow::Result<crate::realtime::RealtimeSettings> {
    let (model,voice) = defaults("openai",crate::locale::is_zh());
    Ok(crate::realtime::RealtimeSettings {
        provider: crate::realtime::Provider::OpenAi, listen_only:true,
        api_key: config::tts_key(cfg).context("No OpenAI key saved")?,
        url:"wss://api.openai.com/v1/realtime".into(),
        model: if cfg.tts_model.is_empty() { model.into() } else { cfg.tts_model.clone() },
        voice: if cfg.tts_voice.is_empty() { voice.into() } else { cfg.tts_voice.clone() },
        instructions: "You are a speech synthesizer. Read exactly the text after 【朗读】 in the original language, without any introduction, answer or explanation.".into(),
        tools:Vec::new(), speech_rate:(1.0+cfg.tts_rate as f32*0.1).clamp(0.5,1.5),
    })
}

fn xml(text: &str) -> String {
    text.replace('&',"&amp;").replace('<',"&lt;").replace('>',"&gt;").replace('"',"&quot;").replace('\'',"&apos;")
}

pub(super) fn request(cfg: &Config, text: &str) -> anyhow::Result<reqwest::RequestBuilder> {
    let key = config::tts_key(cfg).context("No key saved for this speech provider")?;
    let (default_model, default_voice) = defaults(&cfg.tts_provider,crate::locale::is_zh());
    let model = if cfg.tts_model.is_empty() { default_model } else { &cfg.tts_model };
    let voice = if cfg.tts_voice.is_empty() { default_voice } else { &cfg.tts_voice };
    let client = reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(8))
        .timeout(std::time::Duration::from_secs(90)).build()?;
    let speed = (1.0 + cfg.tts_rate as f32 * 0.1).clamp(0.5,1.5);
    Ok(match cfg.tts_provider.as_str() {
        "openai" => client.post("https://api.openai.com/v1/audio/speech").bearer_auth(key)
            .json(&json!({"model":model,"voice":voice,"input":text,"response_format":"pcm","speed":speed})),
        "elevenlabs" => {
            ensure!(!voice.is_empty() && voice.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),"Enter a valid ElevenLabs voice ID");
            client.post(format!("https://api.elevenlabs.io/v1/text-to-speech/{voice}/stream?output_format=pcm_24000")).header("xi-api-key",key)
                .json(&json!({"text":text,"model_id":model,"voice_settings":{"speed":speed.clamp(0.7,1.2)}}))
        }
        "azure" => {
            let region = config::azure_region(cfg);
            ensure!(!region.is_empty() && region.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),"Enter a valid Azure Speech region");
            let language = if voice.starts_with("zh-") { "zh-CN" } else { crate::locale::pick("en-US","zh-CN") };
            let ssml = format!("<speak version=\"1.0\" xmlns=\"http://www.w3.org/2001/10/synthesis\" xml:lang=\"{language}\"><voice name=\"{}\"><prosody rate=\"{:+}%\">{}</prosody></voice></speak>",xml(voice),cfg.tts_rate*10,xml(text));
            client.post(format!("https://{region}.tts.speech.microsoft.com/cognitiveservices/v1"))
                .header("Ocp-Apim-Subscription-Key",key).header("Content-Type","application/ssml+xml")
                .header("X-Microsoft-OutputFormat","raw-24khz-16bit-mono-pcm").header("User-Agent","Aeri").body(ssml)
        }
        _ => bail!("Unsupported speech provider"),
    })
}

// HTTP chunks may split a PCM16 sample at any byte; retain the final byte.
#[derive(Default)]
pub(super) struct PcmChunks { pending: Vec<u8> }
impl PcmChunks {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(chunk);
        let len = self.pending.len() & !1;
        self.pending.drain(..len).collect()
    }
    pub fn complete(&self) -> bool { self.pending.is_empty() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_requests_and_pcm_boundaries() {
        let mut cfg = Config::default();
        for provider in ["openai","elevenlabs","azure"] {
            cfg.tts_provider = provider.into();
            cfg.azure_speech_region = "eastus".into();
            cfg.stt_api_keys_protected.insert(provider.into(),config::protect_key("recorded-test-key").unwrap());
            if provider == "openai" {
                let s = openai_settings(&cfg).unwrap();
                assert_eq!(s.model,"gpt-realtime-2.1-mini");
                assert!(s.listen_only && s.tools.is_empty());
                cfg.tts_model = "gpt-4o-mini-tts".into();
            } else { cfg.tts_model.clear(); }
            let r = request(&cfg,"Hello <Aeri> & friends").unwrap().build().unwrap();
            assert_eq!(r.method(),reqwest::Method::POST);
            let body = r.body().unwrap().as_bytes().unwrap();
            if provider == "azure" {
                let body = String::from_utf8_lossy(body);
                assert!(body.contains("&lt;Aeri&gt; &amp; friends"));
                assert_eq!(r.headers()["X-Microsoft-OutputFormat"],"raw-24khz-16bit-mono-pcm");
            } else {
                let body: serde_json::Value = serde_json::from_slice(body).unwrap();
                if provider == "openai" { assert_eq!(body["response_format"],"pcm"); }
                else { assert!(r.url().as_str().contains("output_format=pcm_24000")); }
            }
            assert!(!r.url().as_str().contains("recorded-test-key"));
        }
        let mut pcm = PcmChunks::default();
        assert_eq!(pcm.push(&[1]),Vec::<u8>::new());
        assert_eq!(pcm.push(&[2,3,4,5]),vec![1,2,3,4]);
        assert!(!pcm.complete());
        assert_eq!(pcm.push(&[6]),vec![5,6]);
        assert!(pcm.complete());
    }
}
