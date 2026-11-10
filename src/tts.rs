//! 语音播报：Windows 自带的 SAPI，跑在独立线程上（COM 对象不跨线程）。可以中途打断。

mod cloud;
pub use cloud::defaults;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;

use tokio::sync::oneshot;
use windows::Win32::Media::Speech::{ISpVoice, ISpObjectTokenCategory, SPF_ASYNC, SPF_PURGEBEFORESPEAK, SPCAT_VOICES, SpObjectTokenCategory, SpVoice};
use windows::Win32::Foundation::WAIT_OBJECT_0;
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree};
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::core::HSTRING;

// Enumerate only SAPI-visible voices. Natural/Neural voices win when Windows
// exposes them through SAPI; otherwise use the best installed language match.
unsafe fn select_language_voice(voice: &ISpVoice) {
    unsafe {
        let Ok(category) = CoCreateInstance::<_, ISpObjectTokenCategory>(&SpObjectTokenCategory, None, CLSCTX_ALL) else { return };
        if category.SetId(SPCAT_VOICES, false).is_err() { return; }
        let Ok(tokens) = category.EnumTokens(None, None) else { return };
        let mut count = 0;
        if tokens.GetCount(&mut count).is_err() { return; }
        let mut best = None;
        let mut best_score = 0;
        for i in 0..count {
            let Ok(token) = tokens.Item(i) else { continue };
            let Ok(attributes) = token.OpenKey(windows::core::w!("Attributes")) else { continue };
            let read = |key| {
                attributes.GetStringValue(key).map(|p| {
                    let text = p.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(p.0.cast()));
                    text
                }).unwrap_or_default()
            };
            let language = read(windows::core::w!("Language"));
            let name = read(windows::core::w!("Name"));
            let score = voice_score(&language, &name, crate::locale::is_zh());
            if score > best_score { best_score = score; best = Some(token); }
        }
        if let Some(token) = best { let _ = voice.SetVoice(&token); }
    }
}

fn voice_score(language: &str, name: &str, zh: bool) -> u8 {
    let matches = language.split(';').filter_map(|id| u16::from_str_radix(id.trim(), 16).ok())
        .any(|id| id & 0x3ff == if zh { 0x04 } else { 0x09 });
    if !matches { return 0; }
    let name = name.to_ascii_lowercase();
    1 + if name.contains("natural") || name.contains("neural") { 2 } else { 0 }
}

type Job = (String, i32, oneshot::Sender<()>);

pub struct Tts {
    tx: mpsc::Sender<Job>,
    stop: Arc<AtomicBool>,
    generation: AtomicU64,
}

impl Tts {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        std::thread::Builder::new()
            .name("tts".into())
            .spawn(move || unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                let voice: Option<ISpVoice> = CoCreateInstance(&SpVoice, None, CLSCTX_ALL).ok();
                if let Some(voice) = &voice { select_language_voice(voice); }
                for (text, rate, done) in rx {
                    if let Some(voice) = &voice {
                        stop2.store(false, Ordering::SeqCst);
                        let _ = voice.SetRate(rate);
                        // 异步朗读，每 50ms 看一次要不要打断
                        if voice.Speak(&HSTRING::from(text), SPF_ASYNC.0 as u32, None).is_ok() {
                            loop {
                                if stop2.load(Ordering::SeqCst) {
                                    let _ = voice.Speak(&HSTRING::new(), (SPF_ASYNC.0 | SPF_PURGEBEFORESPEAK.0) as u32, None);
                                    break;
                                }
                                // 朗读队列读完时这个事件变为有信号
                                if WaitForSingleObject(voice.SpeakCompleteEvent(), 50) == WAIT_OBJECT_0 {
                                    break;
                                }
                            }
                        }
                    }
                    let _ = done.send(());
                }
            })
            .expect("启动播报线程失败");
        Self { tx, stop, generation: AtomicU64::new(0) }
    }

    /// 朗读并等待读完（或被 stop 打断）
    pub async fn speak(&self, text: &str, rate: i32) {
        let (done_tx, done_rx) = oneshot::channel();
        if self.tx.send((text.to_string(), rate, done_tx)).is_ok() {
            let _ = done_rx.await;
        }
    }

    /// Cloud failures fall back to the installed Windows voice. Cancellation never
    /// triggers fallback and drops both the HTTP body and queued PCM immediately.
    pub async fn speak_configured(&self, cfg: &crate::config::Config, text: &str, mut note: impl FnMut(&str)) {
        if cfg.tts_provider == "windows" { self.speak(text,cfg.tts_rate).await; return; }
        let generation = self.generation.load(Ordering::SeqCst);
        let result = self.speak_cloud(cfg,text,generation).await;
        if self.generation.load(Ordering::SeqCst) != generation { return; }
        if result.is_err() {
            note(crate::locale::pick("Cloud speech failed. Using the Windows voice instead; check the speech provider, key, region and voice.", "云端播报失败，已改用 Windows 语音；请检查语音服务、密钥、区域和音色。"));
            self.speak(text,cfg.tts_rate).await;
        }
    }

    async fn speak_cloud(&self, cfg: &crate::config::Config, text: &str, generation: u64) -> anyhow::Result<()> {
        let operation = async {
            let realtime_openai = cfg.tts_provider == "openai" && (cfg.tts_model.is_empty() || cfg.tts_model.starts_with("gpt-realtime"));
            let mut received = false;
            let mut player;
            if realtime_openai {
                let mut session = crate::realtime::Session::connect(&cloud::openai_settings(cfg)?).await?;
                player = tokio::task::spawn_blocking(crate::player::Player::open).await??;
                session.read_aloud(text).await?;
                loop {
                    match session.next_event().await? {
                        crate::realtime::Event::Audio(bytes) => { received |= !bytes.is_empty(); player.push_pcm16(&bytes); }
                        crate::realtime::Event::ResponseDone(status) => { anyhow::ensure!(status == "completed", "Speech generation did not complete"); break; }
                        crate::realtime::Event::Error(_) => anyhow::bail!("OpenAI rejected speech generation"),
                        _ => {}
                    }
                }
                // Dropping the socket also makes cancellation independent of a
                // server acknowledgement; no microphone audio is sent for TTS.
            } else {
                let mut response = cloud::request(cfg,text)?.send().await?;
                anyhow::ensure!(response.status().is_success(), "Speech service returned HTTP {}",response.status());
                player = tokio::task::spawn_blocking(crate::player::Player::open).await??;
                let mut pcm = cloud::PcmChunks::default();
                while let Some(bytes) = response.chunk().await? {
                    let bytes = pcm.push(&bytes);
                    received |= !bytes.is_empty();
                    player.push_pcm16(&bytes);
                }
                anyhow::ensure!(pcm.complete(),"Speech service returned incomplete PCM audio");
            }
            anyhow::ensure!(received,"Speech service returned no audio");
            while player.is_playing() { tokio::time::sleep(std::time::Duration::from_millis(20)).await; }
            Ok(())
        };
        self.until_stopped(operation,generation).await
    }

    async fn until_stopped(&self, operation: impl std::future::Future<Output=anyhow::Result<()>>, generation: u64) -> anyhow::Result<()> {
        let operation = tokio::time::timeout(std::time::Duration::from_secs(90),operation);
        tokio::pin!(operation);
        loop {
            tokio::select! {
                result = &mut operation => return result.map_err(|_| anyhow::anyhow!("Speech generation timed out"))?,
                _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {
                    if self.generation.load(Ordering::SeqCst) != generation { return Ok(()); }
                }
            }
        }
    }

    /// 打断正在进行的朗读
    pub fn stop(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.stop.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cloud_wait_is_cancelled_and_the_pending_stream_is_dropped() {
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag { fn drop(&mut self) { self.0.store(true,Ordering::SeqCst); } }
        let dropped = Arc::new(AtomicBool::new(false));
        let flag = DropFlag(dropped.clone());
        let tts = Tts::new();
        let pending = async move { let _flag = flag; std::future::pending::<()>().await; Ok(()) };
        let generation = tts.generation.load(Ordering::SeqCst);
        let speaking = tts.until_stopped(pending,generation);
        tokio::pin!(speaking);
        tokio::select! {
            _ = &mut speaking => panic!("Stream completed before cancellation"),
            _ = tokio::time::sleep(std::time::Duration::from_millis(30)) => tts.stop(),
        }
        tokio::time::timeout(std::time::Duration::from_millis(200),speaking).await.unwrap().unwrap();
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn language_and_natural_voice_preference() {
        assert_eq!(voice_score("409", "English", false), 1);
        assert_eq!(voice_score("809;409", "English Natural", false), 3);
        assert_eq!(voice_score("804", "Chinese", false), 0);
        assert_eq!(voice_score("804", "Chinese Neural", true), 3);
        assert_eq!(voice_score("409", "English", true), 0);
    }

    /// 会真的出声：念一段长句，0.5 秒后打断，应当很快停下。cargo test -- --ignored
    #[test]
    #[ignore]
    fn stop_interrupts_speech() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let tts = Tts::new();
        rt.block_on(async {
            let start = std::time::Instant::now();
            let speak = tts.speak("这是一段比较长的测试句子，用来检查播报能不能被中途打断，正常情况下要念好几秒钟才能念完。", 0);
            tokio::pin!(speak);
            tokio::select! {
                _ = &mut speak => panic!("还没打断就念完了"),
                _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => tts.stop(),
            }
            speak.await;
            println!("打断后停止，总用时 {:?}", start.elapsed());
            assert!(start.elapsed() < std::time::Duration::from_millis(1500));
        });
    }
}
