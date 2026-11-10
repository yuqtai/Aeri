//! 麦克风采集 + “耳朵”线程：待机时只跑唤醒词检测，唤醒后把音频转发给云端识别。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::Notify;
use tokio::sync::mpsc::UnboundedSender;

use crate::assistant::Command;
use crate::kws::WakeDetector;
use crate::resample::Resampler;

/// 麦克风这一刻在做什么。全局只有一份：电脑麦克风（耳朵线程）和手机麦克风都看它，没有电脑麦克风时手机也能说话
///
/// 转换（谁来切）：
/// - Idle → Capture：耳朵线程听到唤醒词；或调度中心开始一轮（点光球、手机按住说话、追问）
/// - Capture → Muted：说完了，开始思考 / 执行
/// - Muted → Barge：开始播报，且开着“说唤醒词打断”；Barge → Capture：播报时听到唤醒词
/// - 任何 → Idle / Paused：这一轮结束，按“语音唤醒”开关回到待机或暂停
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Mode {
    /// 待机：检测唤醒词
    Idle = 0,
    /// 录音：音频转发给识别
    Capture = 1,
    /// 静音：思考 / 播报期间，丢弃音频
    Muted = 2,
    /// 用户暂停了语音唤醒
    Paused = 3,
    /// 播报中：只听唤醒词，听到就打断播报并开始录音
    Barge = 4,
}

impl Mode {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Idle,
            1 => Self::Capture,
            2 => Self::Muted,
            4 => Self::Barge,
            _ => Self::Paused,
        }
    }
}

static MODE: AtomicU8 = AtomicU8::new(Mode::Paused as u8);

/// While Aeri speaks, talking (not only the wake word) interrupts it
static TALK_BARGE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_talk_barge(on: bool) {
    TALK_BARGE.store(on, Ordering::SeqCst);
}

/// Notices the user starting to talk over Aeri's own voice.
/// The first second of playback teaches it how loud the reply sounds in the microphone
/// (near silent with headphones, clearly audible with speakers); after that, sustained sound
/// well above that level counts as the user talking.
pub(crate) struct TalkDetector {
    heard: usize,
    echo_peak: f32,
    loud_run: usize,
    recent: std::collections::VecDeque<Vec<f32>>,
}

impl TalkDetector {
    const LEARN: usize = 16000; // 1 s at 16 kHz
    const NEED: usize = 4800; // 0.3 s of continuous talking
    const MARGIN: f32 = 3.0; // about 10 dB louder than the reply
    const FLOOR: f32 = 0.012;

    pub(crate) fn new() -> Self {
        Self { heard: 0, echo_peak: 0.0, loud_run: 0, recent: Default::default() }
    }

    /// Returns the audio of the talking so far once it is sure, so no words are lost.
    pub(crate) fn process(&mut self, samples: &[f32]) -> Option<Vec<f32>> {
        if samples.is_empty() {
            return None;
        }
        let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
        self.recent.push_back(samples.to_vec());
        while self.recent.iter().map(Vec::len).sum::<usize>() > 12000 {
            self.recent.pop_front();
        }
        if self.heard < Self::LEARN {
            self.heard += samples.len();
            self.echo_peak = self.echo_peak.max(rms);
            return None;
        }
        if rms > (self.echo_peak * Self::MARGIN).max(Self::FLOOR) {
            self.loud_run += samples.len();
        } else {
            self.loud_run = self.loud_run.saturating_sub(samples.len() / 2);
        }
        (self.loud_run >= Self::NEED).then(|| self.recent.drain(..).flatten().collect())
    }
}

pub fn mode() -> Mode {
    Mode::from_u8(MODE.load(Ordering::SeqCst))
}

pub fn set_mode(mode: Mode) {
    MODE.store(mode as u8, Ordering::SeqCst);
}

// ---------- 手机麦克风 ----------
//
// 手机按住说话时，手机传来的声音代替电脑麦克风送去识别：
// - 按下后这一轮里电脑麦克风的声音不再送去识别（唤醒词照常听）；
// - 还没开始收音（助手正在停下上一轮、连接服务）时先按顺序存着，开始收音时先补发，开头的字不丢；
// - 松开时补一段静音，让服务端判断这句话说完了。

/// 松开说话后补多少静音（16k 采样，1.5 秒）
const PHONE_RELEASE_SILENCE: usize = 16000 * 3 / 2;

/// “听手机”这一轮进行到哪一步。和手指按没按着（held）是两回事：
/// 手指可能在一轮开始前就松开（存着的声音照样要听完），也可能一轮出错结束了还按着。
///
/// None ──按着时 phone_wants_turn──▶ Requested ──phone_begin_turn──▶ Running ──phone_end_turn──▶ None（已松开）
///                                                                                  └─────────▶ Served（还按着）
/// Served：这次按下已经听过一轮，按着不放也不再自动开始（免得出错后反复重试）；下次按下回到 None
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PhoneTurn {
    None,
    /// 已经请调度中心开始，还没开始
    Requested,
    /// 调度中心正在听手机
    Running,
    Served,
}

struct PhoneMic {
    /// 手机正按着说话键
    held: bool,
    turn: PhoneTurn,
    /// 还没开始收音时收到的声音
    pending: PreRoll,
    out: Option<UnboundedSender<Vec<f32>>>,
}

static PHONE: Mutex<PhoneMic> = Mutex::new(PhoneMic {
    held: false,
    turn: PhoneTurn::None,
    pending: PreRoll::new(PREROLL_MAX_SAMPLES),
    out: None,
});

/// 测试里会改手机麦克风的全局状态，用它排队，免得并行的测试互相干扰
#[cfg(test)]
pub(crate) static PHONE_TEST_LOCK: Mutex<()> = Mutex::new(());

fn phone() -> std::sync::MutexGuard<'static, PhoneMic> {
    PHONE.lock().unwrap_or_else(|e| e.into_inner())
}

/// 手机的声音送到哪里（和电脑麦克风同一个通道）
pub fn set_phone_output(tx: UnboundedSender<Vec<f32>>) {
    phone().out = Some(tx);
}

/// 这一轮用的是手机麦克风：电脑麦克风的声音不送去识别
pub fn phone_active() -> bool {
    let p = phone();
    p.held || matches!(p.turn, PhoneTurn::Requested | PhoneTurn::Running)
}

/// 手机按下说话键
pub fn phone_press() {
    let mut p = phone();
    p.held = true;
    if p.turn == PhoneTurn::Served {
        p.turn = PhoneTurn::None;
    }
    p.pending = PreRoll::new(PREROLL_MAX_SAMPLES);
}

/// 手机传来一段声音（16k 单声道）。正在收音就直接送去识别，否则先存着
pub fn phone_audio(samples: Vec<f32>) {
    let mut p = phone();
    if !p.held || samples.is_empty() {
        return;
    }
    p.feed(samples);
}

/// 手机松开说话键：补一段静音让服务端断句
pub fn phone_release() {
    let mut p = phone();
    if !p.held {
        return;
    }
    p.feed(vec![0.0; PHONE_RELEASE_SILENCE]);
    p.held = false;
}

/// 手机按了一下就松开（没打算说话）：丢掉存着的声音。返回“听手机”的这一轮是否已经开始（需要停下）
pub fn phone_cancel() -> bool {
    let mut p = phone();
    p.held = false;
    if p.turn == PhoneTurn::Requested {
        p.turn = PhoneTurn::None;
    }
    p.pending = PreRoll::new(PREROLL_MAX_SAMPLES);
    p.turn == PhoneTurn::Running
}

/// 手机正按着说话键、还没请调度中心开始听：记下“已请求”并返回 true（调用方去发命令）。
/// 同一次按下只会返回一次 true
pub fn phone_wants_turn() -> bool {
    let mut p = phone();
    if p.held && p.turn == PhoneTurn::None {
        p.turn = PhoneTurn::Requested;
        true
    } else {
        false
    }
}

/// 调度中心开始“听手机”的这一轮：先补发存着的声音，再切到收音（同一把锁里做，先后顺序不乱）。
/// 手机已经取消了（没按着、也没存下声音）时返回 false，不用开始
pub fn phone_begin_turn() -> bool {
    let mut p = phone();
    if !p.held && p.pending.samples() == 0 {
        p.turn = PhoneTurn::None;
        return false;
    }
    p.turn = PhoneTurn::Running;
    let pending = std::mem::replace(&mut p.pending, PreRoll::new(PREROLL_MAX_SAMPLES));
    if let Some(out) = &p.out {
        for chunk in pending.into_chunks() {
            let _ = out.send(chunk);
        }
    }
    set_mode(Mode::Capture);
    true
}

/// 回到待机：这一轮结束。手机还按着时保留状态（马上会开始下一轮）
pub fn phone_end_turn() {
    let mut p = phone();
    if p.held {
        if p.turn == PhoneTurn::Running {
            p.turn = PhoneTurn::Served;
        }
    } else {
        p.turn = PhoneTurn::None;
        p.pending = PreRoll::new(PREROLL_MAX_SAMPLES);
    }
}

impl PhoneMic {
    fn feed(&mut self, samples: Vec<f32>) {
        if mode() == Mode::Capture {
            if let Some(out) = &self.out {
                let _ = out.send(samples);
            }
        } else {
            self.pending.push(samples);
        }
    }
}

pub fn input_device_names() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|devs| devs.filter_map(|d| d.description().ok().map(|desc| desc.name().to_string())).collect())
        .unwrap_or_default()
}

fn pick_device(hint: &str) -> anyhow::Result<cpal::Device> {
    let host = cpal::default_host();
    if !hint.is_empty() {
        let hint_lower = hint.to_lowercase();
        if let Ok(devices) = host.input_devices() {
            for d in devices {
                if d.description().is_ok_and(|desc| desc.name().to_lowercase().contains(&hint_lower)) {
                    return Ok(d);
                }
            }
        }
    }
    host.default_input_device().ok_or_else(|| anyhow!("没有找到麦克风"))
}

fn device_name(device: &cpal::Device) -> String {
    device.description().map(|d| d.name().to_string()).unwrap_or_else(|_| "未知设备".into())
}

/// 正在用的麦克风是不是已经不是该用的那个了：
/// 用系统默认时，默认设备换了（插上耳机）；指定了设备但当时没插、用了默认的，现在指定的又插上了。
fn should_switch(hint: &str, current: &str) -> bool {
    let host = cpal::default_host();
    if hint.is_empty() {
        return host.default_input_device().is_some_and(|d| device_name(&d) != current);
    }
    let hint = hint.to_lowercase();
    !current.to_lowercase().contains(&hint)
        && host.input_devices().is_ok_and(|mut ds| ds.any(|d| device_name(&d).to_lowercase().contains(&hint)))
}

/// 打开麦克风，回调里转成单声道 f32 发到 tx。返回（流，采样率，设备名）。
fn open_mic(hint: &str, tx: mpsc::Sender<Vec<f32>>) -> anyhow::Result<(cpal::Stream, u32, String)> {
    let device = pick_device(hint)?;
    let name = device_name(&device);
    let supported = device.default_input_config().context("读取麦克风格式失败")?;
    let channels = supported.channels() as usize;
    let rate = supported.sample_rate();
    let config = supported.config();
    let on_error = |e| eprintln!("麦克风错误：{e}");
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let _ = tx.send(data.chunks(channels).map(|f| f.iter().sum::<f32>() / channels as f32).collect());
            },
            on_error,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                let _ = tx.send(
                    data.chunks(channels)
                        .map(|f| f.iter().map(|&s| s as f32 / 32768.0).sum::<f32>() / channels as f32)
                        .collect(),
                );
            },
            on_error,
            None,
        ),
        other => return Err(anyhow!("不支持的麦克风采样格式：{other:?}")),
    }
    .context("打开麦克风失败")?;
    stream.play().context("启动录音失败")?;
    Ok((stream, rate, name))
}

/// 能量门限：跟踪底噪，只有声音明显高于底噪时才把音频交给唤醒词模型，安静时几乎不占 CPU。
/// 触发时先补上之前缓存的 0.5 秒，唤醒词开头不会被切掉；声音消失 2 秒后重新休眠。
pub(crate) struct EnergyGate {
    noise_floor: f32,
    active_samples_left: usize,
    history: std::collections::VecDeque<Vec<f32>>,
    history_len: usize,
}

impl EnergyGate {
    const PRE_ROLL: usize = 8000; // 0.5 秒
    const HANGOVER: usize = 32000; // 2 秒

    pub(crate) fn new() -> Self {
        Self { noise_floor: 0.002, active_samples_left: 0, history: Default::default(), history_len: 0 }
    }

    /// 返回需要送进唤醒词模型的音频片段（可能为空）
    pub(crate) fn process(&mut self, samples: &[f32], detector: &mut WakeDetector) -> Vec<Vec<f32>> {
        if samples.is_empty() {
            return Vec::new();
        }
        let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
        let loud = rms > (self.noise_floor * 3.0).max(0.0015);
        if !loud {
            // 只在安静时慢慢跟踪底噪
            self.noise_floor = self.noise_floor * 0.98 + rms * 0.02;
        }
        if loud {
            let was_sleeping = self.active_samples_left == 0;
            self.active_samples_left = Self::HANGOVER;
            if was_sleeping {
                detector.reset();
                let mut out: Vec<Vec<f32>> = self.history.drain(..).collect();
                self.history_len = 0;
                out.push(samples.to_vec());
                return out;
            }
        }
        if self.active_samples_left > 0 {
            self.active_samples_left = self.active_samples_left.saturating_sub(samples.len());
            return vec![samples.to_vec()];
        }
        self.history.push_back(samples.to_vec());
        self.history_len += samples.len();
        while self.history_len > Self::PRE_ROLL {
            if let Some(front) = self.history.pop_front() {
                self.history_len -= front.len();
            }
        }
        Vec::new()
    }
}

/// 唤醒后、实时连接就绪前最多保留多少音频（16k 采样，25 秒）：
/// 覆盖最坏的准备窗口（打开扬声器 ≤5 秒 + 连接整体 ≤15 秒）并留余量，正常情况下不会裁剪，只作内存保护
pub const PREROLL_MAX_SAMPLES: usize = 16000 * 25;

/// 唤醒后先录下的音频：按到达顺序保存，超过上限丢最旧的整块（不切开块）
pub struct PreRoll {
    chunks: std::collections::VecDeque<Vec<f32>>,
    samples: usize,
    max: usize,
}

impl PreRoll {
    pub const fn new(max: usize) -> Self {
        Self { chunks: std::collections::VecDeque::new(), samples: 0, max }
    }

    pub fn push(&mut self, chunk: Vec<f32>) {
        if chunk.is_empty() {
            return;
        }
        self.samples += chunk.len();
        self.chunks.push_back(chunk);
        while self.samples > self.max {
            let Some(front) = self.chunks.pop_front() else { break };
            self.samples -= front.len();
        }
    }

    pub fn samples(&self) -> usize {
        self.samples
    }

    pub fn into_chunks(self) -> impl Iterator<Item = Vec<f32>> {
        self.chunks.into_iter()
    }
}

pub struct EarSettings {
    pub device: String,
    pub wake_words: Vec<String>,
    pub threshold: f32,
    pub score: f32,
    pub model_dir: PathBuf,
}

/// 打不开麦克风（没插、被占用、格式不支持）；和唤醒词模型的问题区分开，只有这种才值得定时重试
#[derive(Debug)]
pub struct MicUnavailable;

impl std::fmt::Display for MicUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("麦克风不可用")
    }
}

pub fn is_mic_unavailable(e: &anyhow::Error) -> bool {
    e.downcast_ref::<MicUnavailable>().is_some()
}

/// 多久没收到音频就认为麦克风断开了
const SILENT_DEVICE_TIMEOUT: Duration = Duration::from_secs(3);
/// 多久检查一次该用的麦克风有没有变化
const DEVICE_CHECK_EVERY: Duration = Duration::from_secs(5);

/// 后台“耳朵”线程的句柄；drop 时停止线程并释放麦克风。模式见全局的 mode() / set_mode()
pub struct Ear {
    /// 播报中听到唤醒词时通知（这时已经切到录音模式）
    pub barge: Arc<Notify>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Ear {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// 启动耳朵线程。成功时返回（句柄，设备名，唤醒词问题说明）。
pub fn start_ear(
    settings: EarSettings,
    initial_mode: Mode,
    events: UnboundedSender<Command>,
    audio_out: UnboundedSender<Vec<f32>>,
) -> anyhow::Result<(Ear, String, Vec<String>)> {
    set_mode(initial_mode);
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::channel::<anyhow::Result<(String, Vec<String>)>>();
    let barge = Arc::new(Notify::new());
    let (stop2, barge2) = (stop.clone(), barge.clone());

    let thread = std::thread::Builder::new()
        .name("ear".into())
        .spawn(move || {
            // 模型和麦克风都在本线程里创建（cpal 的流不能跨线程）
            let detector = WakeDetector::new(&settings.model_dir, &settings.wake_words, settings.threshold, settings.score);
            let (mut detector, problems) = match detector {
                Ok(v) => v,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let (raw_tx, raw_rx) = mpsc::channel();
            let (_stream, rate, name) = match open_mic(&settings.device, raw_tx) {
                Ok(v) => v,
                Err(e) => {
                    let _ = ready_tx.send(Err(e.context(MicUnavailable)));
                    return;
                }
            };
            let _ = ready_tx.send(Ok((name.clone(), problems)));

            let mut resampler = Resampler::new(rate);
            let mut gate = EnergyGate::new();
            let mut talk = TalkDetector::new();
            let mut last_mode: Option<Mode> = None;
            // 正常的麦克风即使安静也会持续回调；几秒没有数据说明设备被拔掉或出错了
            let mut last_audio = Instant::now();
            let mut last_device_check = Instant::now();
            while !stop2.load(Ordering::SeqCst) {
                if last_device_check.elapsed() > DEVICE_CHECK_EVERY {
                    last_device_check = Instant::now();
                    if should_switch(&settings.device, &name) {
                        let _ = events.send(Command::MicChanged);
                        return;
                    }
                }
                let Ok(chunk) = raw_rx.recv_timeout(Duration::from_millis(200)) else {
                    if last_audio.elapsed() > SILENT_DEVICE_TIMEOUT {
                        let _ = events.send(Command::MicChanged);
                        return;
                    }
                    continue;
                };
                last_audio = Instant::now();
                let samples = resampler.process(&chunk);
                let current = mode();
                if current == Mode::Idle || current == Mode::Barge {
                    if last_mode != Some(current) {
                        detector.reset();
                        gate = EnergyGate::new();
                        talk = TalkDetector::new();
                    }
                    if current == Mode::Barge && TALK_BARGE.load(Ordering::SeqCst) {
                        if let Some(spoken) = talk.process(&samples) {
                            set_mode(Mode::Capture);
                            barge2.notify_one();
                            let _ = audio_out.send(spoken);
                            last_mode = Some(Mode::Capture);
                            continue;
                        }
                    }
                    // 安静时不跑唤醒词模型；有声音时连同之前 0.5 秒一起送进去
                    for piece in gate.process(&samples, &mut detector) {
                        if let Some(word) = detector.accept(&piece) {
                            // 立刻切到录音，唤醒词之后的话一个字都不丢
                            set_mode(Mode::Capture);
                            if current == Mode::Barge {
                                barge2.notify_one();
                            } else {
                                let _ = events.send(Command::Wake(word));
                            }
                            break;
                        }
                    }
                } else if current == Mode::Capture && !samples.is_empty() && !phone_active() {
                    // 这一轮用手机麦克风时，电脑麦克风的声音不送去识别
                    let _ = audio_out.send(samples);
                }
                last_mode = Some(current);
            }
        })?;

    match ready_rx.recv_timeout(Duration::from_secs(15)) {
        Ok(Ok((name, problems))) => Ok((Ear { barge, stop, thread: Some(thread) }, name, problems)),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(anyhow!("启动麦克风超时").context(MicUnavailable)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 按着不放时这一轮出错结束了：不能自动反复开始
    #[test]
    fn phone_press_starts_at_most_one_turn() {
        let _serial = PHONE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        phone_press();
        assert!(phone_wants_turn());
        assert!(!phone_wants_turn(), "同一次按下只请求一次");
        assert!(phone_begin_turn());
        phone_end_turn(); // 这一轮出错结束，手指还按着
        assert!(phone_active());
        assert!(!phone_wants_turn(), "这次按下已经开始过一轮");
        phone_release();
        phone_end_turn();
        assert!(!phone_active());
        // 下一次按下又可以开始
        phone_press();
        assert!(phone_wants_turn());
        assert!(!phone_cancel(), "还没开始的一轮不用停");
        assert!(!phone_begin_turn(), "取消后不用开始");
        phone_end_turn();
        set_mode(Mode::Paused);
    }

    /// 一轮还没开始就松开了：存着的声音照样要听完，这期间仍算“在用手机麦克风”
    #[test]
    fn phone_release_before_turn_still_runs_it() {
        let _serial = PHONE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_mode(Mode::Muted);
        phone_press();
        assert!(phone_wants_turn());
        phone_audio(vec![0.1; 100]);
        phone_release();
        assert!(phone_active(), "请求了还没开始，电脑麦克风先别抢");
        assert!(phone_begin_turn(), "有存着的声音，要开始");
        assert_eq!(mode(), Mode::Capture);
        phone_end_turn();
        assert!(!phone_active());
        assert!(!phone_wants_turn(), "手指没按着，不会再请求");
        set_mode(Mode::Paused);
    }

    #[test]
    fn preroll_keeps_order_and_everything_under_limit() {
        let mut p = PreRoll::new(1000);
        for i in 0..5 {
            p.push(vec![i as f32; 100]);
        }
        p.push(Vec::new());
        let firsts: Vec<f32> = p.into_chunks().map(|c| c[0]).collect();
        assert_eq!(firsts, vec![0.0, 1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn preroll_is_bounded_and_drops_oldest_whole_chunks() {
        let mut p = PreRoll::new(250);
        for i in 0..10 {
            p.push(vec![i as f32; 100]);
        }
        assert!(p.samples() <= 250);
        let chunks: Vec<Vec<f32>> = p.into_chunks().collect();
        // 保留最新的两整块，块不被切开
        assert_eq!(chunks.len(), 2);
        assert!(chunks.iter().all(|c| c.len() == 100));
        assert_eq!((chunks[0][0], chunks[1][0]), (8.0, 9.0));
    }
}
