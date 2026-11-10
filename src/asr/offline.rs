//! Offline model presets. Archives are pinned official sherpa-onnx releases.
use super::*;
pub const IDS: [&str;7] = ["zipformer","moonshine","whisper-tiny","whisper-base","whisper-small","sensevoice","parakeet"];

pub(super) fn model(id: &str) -> LocalModel {
    match id {
        "moonshine" => LocalModel { name: "sherpa-onnx-moonshine-tiny-en-int8", sha256: "d5fe6ec4334fef36255b2a4010412cad4c007e33103fec62fb5d17cad88086f2", files: &["cached_decode.int8.onnx", "uncached_decode.int8.onnx", "tokens.txt", "preprocess.onnx", "encode.int8.onnx"] },
        "whisper-tiny" => LocalModel { name: "sherpa-onnx-whisper-tiny", sha256: "c46116994e539aa165266d96b325252728429c12535eb9d8b6a2b10f129e66b1", files: &["tiny-decoder.int8.onnx", "tiny-encoder.int8.onnx", "tiny-tokens.txt"] },
        "whisper-base" => LocalModel { name: "sherpa-onnx-whisper-base", sha256: "911b2083efd7c0dca2ac3b358b75222660dc09fb716d64fbfc417ba6c99ff3de", files: &["base-decoder.int8.onnx", "base-encoder.int8.onnx", "base-tokens.txt"] },
        "whisper-small" => LocalModel { name: "sherpa-onnx-whisper-small", sha256: "486a46afbb7ba798507190ffe02fea2dd726049af212e774537efac6afb210a6", files: &["small-decoder.int8.onnx", "small-tokens.txt", "small-encoder.int8.onnx"] },
        "sensevoice" => LocalModel { name: "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09", sha256: "7305f7905bfcf77fa0b39388a313f3da35c68d971661a65475b56fb2162c8e63", files: &["tokens.txt", "model.int8.onnx"] },
        "parakeet" => LocalModel { name: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8", sha256: "6fc4426b54aa549a0019b8a75b8dea59164e71790a37ac56a045ad57f915f714", files: &["tokens.txt", "joiner.int8.onnx", "decoder.int8.onnx", "encoder.int8.onnx"] },
        _ => local_model(crate::locale::is_zh()),
    }
}

pub(super) fn recognizer(id: &str, dir: &std::path::Path) -> anyhow::Result<sherpa_onnx::OfflineRecognizer> {
    let mut c = sherpa_onnx::OfflineRecognizerConfig::default();
    let file = |f: &str| Some(dir.join(f).to_string_lossy().into_owned());
    c.model_config.tokens = file("tokens.txt");
    c.model_config.num_threads = 2;
    c.decoding_method = Some("greedy_search".into());
    match id {
        "moonshine" => {
            c.model_config.moonshine.preprocessor = file("preprocess.onnx");
            c.model_config.moonshine.encoder = file("encode.int8.onnx");
            c.model_config.moonshine.uncached_decoder = file("uncached_decode.int8.onnx");
            c.model_config.moonshine.cached_decoder = file("cached_decode.int8.onnx");
        }
        "sensevoice" => {
            c.model_config.sense_voice.model = file("model.int8.onnx");
            c.model_config.sense_voice.language = Some("auto".into());
            c.model_config.sense_voice.use_itn = true;
        }
        "parakeet" => {
            c.model_config.transducer.encoder = file("encoder.int8.onnx");
            c.model_config.transducer.decoder = file("decoder.int8.onnx");
            c.model_config.transducer.joiner = file("joiner.int8.onnx");
            c.model_config.model_type = Some("nemo_transducer".into());
        }
        id if id.starts_with("whisper-") => {
            let size = id.trim_start_matches("whisper-");
            c.model_config.whisper.encoder = file(&format!("{size}-encoder.int8.onnx"));
            c.model_config.whisper.decoder = file(&format!("{size}-decoder.int8.onnx"));
            c.model_config.tokens = file(&format!("{size}-tokens.txt"));
            // Let multilingual Whisper detect the spoken language independently
            // of the interface language, so bilingual speech remains usable.
            c.model_config.whisper.language = None;
            c.model_config.whisper.task = Some("transcribe".into());
            c.model_config.whisper.tail_paddings = -1;
        }
        _ => bail!("Unknown offline model"),
    }
    sherpa_onnx::OfflineRecognizer::create(&c).context("Could not load the offline speech model")
}

pub(super) async fn recognize(s: &AsrSettings, audio: &mut UnboundedReceiver<Vec<f32>>, mut partial: impl FnMut(&str)) -> anyhow::Result<Option<String>> {
    let dir = prepare_local(&s.local_model,|text| partial(text)).await?;
    let id = s.local_model.clone();
    let recognizer = tokio::task::spawn_blocking(move || recognizer(&id,&dir)).await??;
    let samples = collect_utterance(s,audio).await;
    if samples.is_empty() { return Ok(None); }
    let text = tokio::task::spawn_blocking(move || {
        let stream = recognizer.create_stream();
        stream.accept_waveform(16000,&samples);
        recognizer.decode(&stream);
        stream.get_result().map(|r| r.text).unwrap_or_default()
    }).await?;
    partial(&text);
    Ok((!text.trim().is_empty()).then(|| text.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preset_manifest_is_pinned_and_local() {
        for id in IDS {
            let m = model(id);
            assert_eq!(m.sha256.len(),64);
            assert!(m.files.iter().all(|f| !f.contains('/') && !f.contains("..")));
        }
    }
    // Optional official samples exercise the real C API and print reproducible
    // load/decode measurements. No network or microphone is used by this test.
    #[test]
    fn official_offline_samples_when_available() {
        let Ok(root) = std::env::var("AERI_OFFLINE_MODEL_TEST_ROOT") else { return };
        for id in IDS.into_iter().filter(|id| *id != "zipformer") {
            let m = model(id);
            let dir = std::path::Path::new(&root).join(m.name);
            let start = Instant::now();
            let recognizer = recognizer(id,&dir).unwrap();
            let load = start.elapsed().as_secs_f64();
            let sample = if id == "sensevoice" { "test_wavs/zh.wav" } else { "test_wavs/0.wav" };
            let wav = sherpa_onnx::Wave::read(dir.join(sample).to_str().unwrap()).unwrap();
            let duration = wav.samples().len() as f64 / wav.sample_rate() as f64;
            let stream = recognizer.create_stream();
            stream.accept_waveform(wav.sample_rate(),wav.samples());
            let start = Instant::now();
            recognizer.decode(&stream);
            let elapsed = start.elapsed().as_secs_f64();
            let text = stream.get_result().unwrap().text;
            assert!(!text.trim().is_empty(),"{id} must transcribe its official sample");
            println!("MODEL_BENCH {id} load={load:.3}s decode={elapsed:.3}s audio={duration:.3}s rtf={:.3} text={text}",elapsed/duration);
        }
    }
}
