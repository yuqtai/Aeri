//! 播放实时语音模型返回的音频（24kHz、16 位、单声道 PCM）：转成扬声器的采样率和声道数后放进缓冲区，
//! 输出回调从缓冲区取。可以随时清空（被打断时）。只在实时对话期间打开，结束就释放音频设备。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub const SOURCE_RATE: u32 = 24000;
/// 缓冲区非空但超过这个毫秒数仍没有任何样本被输出回调消费，视为播放设备已断流
const STALL_TIMEOUT_MS: u64 = 1500;

fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    // 从非零偏移开始，避免启动最初 1.5 秒内做时间戳减法时饱和到 0
    10_000 + START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

pub struct Player {
    buffer: Arc<Mutex<VecDeque<f32>>>,
    /// 扬声器采样率（单声道样本数换算用）
    rate: u32,
    /// 24k → 扬声器采样率的插值位置，跨块连续
    pos: f64,
    last: f32,
    failed: Arc<AtomicBool>,
    last_drain_ms: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    pub fn open() -> anyhow::Result<Self> {
        let buffer: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
        let failed = Arc::new(AtomicBool::new(false));
        let last_drain_ms = Arc::new(AtomicU64::new(now_ms()));
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel::<anyhow::Result<u32>>();
        let (buf2, failed2, drain2, stop2) = (buffer.clone(), failed.clone(), last_drain_ms.clone(), stop.clone());
        // cpal 的流不能跨线程，放在自己的线程里一直持有
        let thread = std::thread::Builder::new().name("player".into()).spawn(move || {
            let stream = match open_output(buf2, failed2, drain2) {
                Ok((stream, rate)) => {
                    let _ = ready_tx.send(Ok(rate));
                    stream
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            while !stop2.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(100));
            }
            drop(stream);
        })?;
        let rate = ready_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| anyhow!(crate::locale::pick("Timed out opening the speaker", "打开扬声器超时")))??;
        Ok(Self { buffer, rate, pos: 0.0, last: 0.0, failed, last_drain_ms, stop, thread: Some(thread) })
    }

    /// 放入一段 24k PCM16 小端字节
    pub fn push_pcm16(&mut self, bytes: &[u8]) {
        if self.failed.load(Ordering::SeqCst) {
            return;
        }
        let samples: Vec<f32> = bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
        if samples.is_empty() {
            return;
        }
        let step = SOURCE_RATE as f64 / self.rate as f64;
        let mut out = Vec::with_capacity((samples.len() as f64 / step) as usize + 2);
        // 线性插值；pos 是相对 [last, s0, s1, ...] 的位置
        let len = samples.len() as f64;
        while self.pos < len {
            let i = self.pos.floor() as usize;
            let frac = (self.pos - i as f64) as f32;
            let a = if i == 0 { self.last } else { samples[i - 1] };
            let b = samples[i];
            out.push(a + (b - a) * frac);
            self.pos += step;
        }
        self.pos -= len;
        self.last = *samples.last().unwrap();
        let mut buf = self.buffer.lock().unwrap_or_else(|e| e.into_inner());
        if buf.is_empty() {
            self.last_drain_ms.store(now_ms(), Ordering::SeqCst);
        }
        buf.extend(out);
    }

    /// 被打断时丢掉还没播的声音
    pub fn clear(&mut self) {
        self.buffer.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    pub fn is_playing(&self) -> bool {
        let mut buf = self.buffer.lock().unwrap_or_else(|e| e.into_inner());
        if buf.is_empty() {
            return false;
        }
        if self.failed.load(Ordering::SeqCst)
            || now_ms().saturating_sub(self.last_drain_ms.load(Ordering::SeqCst)) > STALL_TIMEOUT_MS
        {
            self.failed.store(true, Ordering::SeqCst);
            buf.clear();
            return false;
        }
        true
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn open_output(
    buffer: Arc<Mutex<VecDeque<f32>>>,
    failed: Arc<AtomicBool>,
    last_drain_ms: Arc<AtomicU64>,
) -> anyhow::Result<(cpal::Stream, u32)> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or_else(|| anyhow!(crate::locale::pick("No speaker found", "没有找到扬声器")))?;
    let supported = device
        .default_output_config()
        .context(crate::locale::pick("Could not read speaker format", "读取扬声器格式失败"))?;
    let channels = supported.channels() as usize;
    let rate = supported.sample_rate();
    let config = supported.config();
    let on_error = move |e| {
        failed.store(true, Ordering::SeqCst);
        eprintln!("{}{e}", crate::locale::pick("Speaker error: ", "扬声器错误："));
    };
    // 每帧取一个单声道样本复制到所有声道，缓冲区空了就输出静音
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => device.build_output_stream(
            config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let mut buf = buffer.lock().unwrap_or_else(|e| e.into_inner());
                let mut drained = false;
                for frame in data.chunks_mut(channels) {
                    let s = match buf.pop_front() {
                        Some(v) => {
                            drained = true;
                            v
                        }
                        None => 0.0,
                    };
                    frame.iter_mut().for_each(|x| *x = s);
                }
                if drained {
                    last_drain_ms.store(now_ms(), Ordering::SeqCst);
                }
            },
            on_error,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_output_stream(
            config,
            move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                let mut buf = buffer.lock().unwrap_or_else(|e| e.into_inner());
                let mut drained = false;
                for frame in data.chunks_mut(channels) {
                    let s = match buf.pop_front() {
                        Some(v) => {
                            drained = true;
                            v
                        }
                        None => 0.0,
                    };
                    let sample = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                    frame.iter_mut().for_each(|x| *x = sample);
                }
                if drained {
                    last_drain_ms.store(now_ms(), Ordering::SeqCst);
                }
            },
            on_error,
            None,
        ),
        other => {
            return Err(anyhow!(
                "{}{other:?}",
                crate::locale::pick("Unsupported speaker sample format: ", "不支持的扬声器采样格式：")
            ));
        }
    }
    .context(crate::locale::pick("Could not open the speaker", "打开扬声器失败"))?;
    stream.play().context(crate::locale::pick("Could not start playback", "启动播放失败"))?;
    Ok((stream, rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headless_player() -> Player {
        Player {
            buffer: Arc::new(Mutex::new(VecDeque::new())),
            rate: 24000,
            pos: 0.0,
            last: 0.0,
            failed: Arc::new(AtomicBool::new(false)),
            last_drain_ms: Arc::new(AtomicU64::new(now_ms())),
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
        }
    }

    #[test]
    fn stream_error_or_stall_clears_buffer_instead_of_hanging() {
        let mut p = headless_player();
        p.push_pcm16(&[0x00, 0x20, 0x00, 0x20, 0x00, 0x20, 0x00, 0x20]);
        assert!(p.is_playing());

        // 模拟输出设备报错断开
        p.failed.store(true, Ordering::SeqCst);
        assert!(!p.is_playing());
        assert!(p.buffer.lock().unwrap().is_empty());

        // 模拟蓝牙耳机静默断流（未触发 on_error，但超过 1500ms 没有样本被回调取走）
        let mut p2 = headless_player();
        let _ = now_ms();
        std::thread::sleep(Duration::from_millis(10));
        p2.push_pcm16(&[0x00, 0x20, 0x00, 0x20]);
        assert!(p2.is_playing());
        p2.last_drain_ms.store(now_ms().saturating_sub(STALL_TIMEOUT_MS + 100), Ordering::SeqCst);
        assert!(!p2.is_playing());
        assert!(p2.buffer.lock().unwrap().is_empty());
    }
}
