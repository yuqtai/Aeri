//! 调度中心：前台负责对话，复杂任务交给后台智能体（Codex / Claude Code / 内置助手），做完主动播报。
//! 前台有两种：
//! - 实时语音（默认）：唤醒后麦克风音频直接发给百炼实时语音模型，边说边回，可以插话；
//! - 识别 + 大模型：fun-asr 识别一句话 → 本地直达规则 / DeepSeek → 系统播报。
//! 两种都支持播报时说唤醒词打断。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::{Notify, oneshot};
use tokio::task::JoinHandle;

use crate::agent::{Agent, FRONT_PROMPT};
use crate::asr::{self, AsrSettings};
use crate::audio::{self, Ear, EarSettings, Mode};
use crate::config::{self, Config};
use crate::duck::Ducker;
use crate::llm::Llm;
use crate::player::Player;
use crate::realtime::{self, RealtimeSettings, VOICE_PROMPT};
use crate::reminders::Reminders;
use crate::tasks::{TaskDone, TaskManager, TaskSettings};
use crate::tools::Tools;
use crate::tools::apps::AppIndex;
use crate::tts::Tts;
use crate::{fastpath, screen, sys};

pub enum Command {
    /// 检测到唤醒词（来自耳朵线程）
    Wake(String),
    /// 界面上点了“说话”
    Talk,
    /// 手机按住说话：这一轮听手机传来的声音（见 audio 里的手机麦克风）
    PhoneTalk,
    /// 界面上输入的文字命令（后台会显示用户气泡）
    Text(String),
    /// 界面已经显示了用户气泡的文字命令。generation 取自提交时的 text_generation()；
    /// 之后若调用过 cancel_pending_texts()，这条还没开始处理就会被跳过
    TextSubmitted { text: String, generation: u64 },
    SetListening(bool),
    /// 麦克风断开、默认麦克风换了，或者指定的麦克风重新插上了（来自耳朵线程）
    MicChanged,
    ClearHistory,
    Reload(Config),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Status {
    Idle = 0,
    Listening = 1,
    Thinking = 2,
    Acting = 3,
    Speaking = 4,
    Paused = 5,
}

#[derive(Clone, Copy, Debug)]
pub enum Role {
    User = 0,
    Assistant = 1,
    Tool = 2,
    System = 3,
}

pub enum UiEvent {
    Status(Status),
    Partial(String),
    Chat(Role, String),
    /// 后台任务的最新进展：连续的进展只占一行，原地更新
    TaskProgress(u32, String),
    /// 是否有后台任务在跑
    TaskRunning(bool),
    Mic(String),
    /// 取出了一条 TextSubmitted（随后处理，或因已取消而跳过），带它提交时的 generation；
    /// 界面只在它等于当前 text_generation() 时减少排队数
    TextStarted(u64),
}

pub type UiSink = Arc<dyn Fn(UiEvent) + Send + Sync>;

/// 停止信号：代次 + 唤醒。每轮开始时记下代次，之后只要代次变了这一轮就算被停止——
/// 即使点停止时没有任何地方在等（比如正在打开扬声器、连接服务），状态也不会丢；
/// 而下一轮会记下新的代次，不会被上一轮的停止误伤（不像 notify_one 会留一个许可）。
pub(crate) struct StopSignal {
    generation: AtomicU64,
    notify: Notify,
}

impl StopSignal {
    pub(crate) const fn new() -> Self {
        Self { generation: AtomicU64::new(0), notify: Notify::const_new() }
    }

    pub(crate) fn stop(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// 开始新的一轮
    pub(crate) fn turn(&'static self) -> Turn {
        Turn { signal: self, generation: self.generation.load(Ordering::SeqCst) }
    }
}

/// 一轮操作（一次命令处理）；被停止后一直保持停止状态
#[derive(Clone, Copy)]
pub(crate) struct Turn {
    signal: &'static StopSignal,
    generation: u64,
}

impl Turn {
    pub(crate) fn cancelled(&self) -> bool {
        self.signal.generation.load(Ordering::SeqCst) != self.generation
    }

    /// 等到这一轮被停止；已经停止时立刻返回
    pub(crate) async fn stopped(self) {
        loop {
            let notified = self.signal.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable(); // 先登记再检查，检查和等待之间的停止也不会漏
            if self.cancelled() {
                return;
            }
            notified.await;
        }
    }

    /// 跑一段准备工作；中途被停止时返回 None（准备工作随之丢弃）
    pub(crate) async fn guard<T>(self, fut: impl std::future::Future<Output = T>) -> Option<T> {
        if self.cancelled() {
            return None;
        }
        tokio::select! {
            biased;
            _ = self.stopped() => None,
            v = fut => if self.cancelled() { None } else { Some(v) },
        }
    }
}

/// 界面上点了“停止”：正在听、思考、执行或回答时立刻停下，回到待机。
static STOP: StopSignal = StopSignal::new();

pub fn interrupt() {
    STOP.stop();
}

/// 文字排队的代次：cancel_pending_texts() 之后，此前提交、还没开始处理的 TextSubmitted 都会被跳过
static TEXT_GENERATION: AtomicU64 = AtomicU64::new(0);

pub fn text_generation() -> u64 {
    TEXT_GENERATION.load(Ordering::SeqCst)
}

pub fn cancel_pending_texts() {
    TEXT_GENERATION.fetch_add(1, Ordering::SeqCst);
}

fn text_is_stale(generation: u64) -> bool {
    generation < text_generation()
}

/// 麦克风不可用时多久重试一次
const MIC_RETRY: Duration = Duration::from_secs(5);

/// 实时对话里，唤醒后多久没开口就结束；回复完多久没再说话就结束
const REALTIME_FOLLOW_UP: Duration = Duration::from_secs(8);
/// 播报结束后再等一会儿才重新收音，避免扬声器余音被当成用户说话
const ECHO_TAIL: Duration = Duration::from_millis(500);

struct Assistant {
    cfg: Config,
    ui: UiSink,
    cmd_tx: UnboundedSender<Command>,
    apps: Arc<AppIndex>,
    tasks: Arc<TaskManager>,
    reminders: Arc<Reminders>,
    done_rx: UnboundedReceiver<TaskDone>,
    /// 前台工具：快速本地工具 + 后台任务
    front_tools: Arc<Tools>,
    /// 识别 + 大模型模式下的前台对话助手
    agent: Option<Agent>,
    tts: Tts,
    ear: Option<Ear>,
    audio_tx: UnboundedSender<Vec<f32>>,
    audio_rx: UnboundedReceiver<Vec<f32>>,
    listening: bool,
    /// 当前这一轮；被用户点了停止就不要再接着听、也不要降级
    turn: Turn,
    /// 上次打开麦克风失败是因为没有可用的麦克风（定时重试）
    mic_missing: bool,
    ducker: Ducker,
    /// 这一轮听的是手机麦克风（不响提示音，电脑没有麦克风也能进行）
    phone: bool,
}

pub async fn run(cfg: Config, mut rx: UnboundedReceiver<Command>, cmd_tx: UnboundedSender<Command>, ui: UiSink) {
    let apps = Arc::new(AppIndex::load_cached());
    {
        let apps = apps.clone();
        tokio::task::spawn_blocking(move || apps.refresh());
    }
    let (audio_tx, audio_rx) = unbounded_channel();
    audio::set_phone_output(audio_tx.clone());
    let (done_tx, done_rx) = unbounded_channel();
    let reminders = Reminders::new(done_tx.clone());
    let tasks = TaskManager::new(done_tx, ui.clone());
    let mut a = Assistant {
        front_tools: Arc::new(Tools::local_only(apps.clone())), // 马上由 apply_config 换成正式的
        listening: cfg.listen_on_start,
        turn: STOP.turn(),
        mic_missing: false,
        ducker: Ducker::start(),
        phone: false,
        agent: None,
        cfg,
        ui,
        cmd_tx,
        apps,
        tasks,
        reminders,
        done_rx,
        tts: Tts::new(),
        ear: None,
        audio_tx,
        audio_rx,
    };
    a.apply_config();
    if let Err(e) = a.restart_ear() {
        a.chat(Role::System, format!("语音唤醒不可用：{e:#}"));
    }
    a.go_idle();
    let mut mic_retry = tokio::time::interval(MIC_RETRY);

    loop {
        tokio::select! {
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { break };
                a.turn = STOP.turn();
                match cmd {
                    Command::Wake(_word) => {
                        // 耳朵检测到唤醒词时会立刻切到录音模式；如果此后模式被改过（比如期间在处理别的命令），这个唤醒已过时
                        let fresh = a.ear.is_some() && a.mode() == Mode::Capture;
                        if a.listening && fresh {
                            #[cfg(debug_assertions)]
                            println!("[唤醒] {_word}");
                            a.conversation(None, Origin::Wake).await;
                        } else if a.mode() == Mode::Capture {
                            // 耳朵已切到录音但这次唤醒不算数：恢复模式并丢掉已录下的，免得堆积或串入下一轮
                            a.go_idle();
                        }
                    }
                    Command::Talk => a.conversation(None, Origin::Manual).await,
                    Command::PhoneTalk => {
                        // 手机按一下就松开了（没存下声音）时不用开始
                        if audio::phone_begin_turn() {
                            a.phone = true;
                            a.conversation(None, Origin::Phone).await;
                        } else {
                            a.go_idle();
                        }
                    }
                    Command::Text(text) => a.text_command(&text, true).await,
                    Command::TextSubmitted { text, generation } => {
                        a.emit(UiEvent::TextStarted(generation));
                        if !text_is_stale(generation) {
                            a.text_command(&text, false).await;
                        }
                    }
                    Command::SetListening(on) => {
                        a.listening = on;
                        a.go_idle();
                    }
                    Command::MicChanged => {
                        let was_ok = a.ear.is_some();
                        a.ear = None; // 先释放旧的
                        match a.restart_ear() {
                            Ok(name) if !was_ok => a.chat(Role::System, format!("麦克风已恢复：{name}")),
                            Ok(_) => {}
                            Err(_) => a.chat(Role::System, "麦克风断开了，重新接上后会自动恢复；现在可以打字使用。"),
                        }
                        a.go_idle();
                    }
                    Command::ClearHistory => {
                        if let Some(agent) = a.agent.as_mut() {
                            agent.clear();
                        }
                    }
                    Command::Reload(new_cfg) => a.reload(new_cfg),
                }
            }
            done = a.done_rx.recv() => {
                if let Some(done) = done {
                    a.turn = STOP.turn();
                    a.announce(done).await;
                }
            }
            _ = mic_retry.tick(), if a.ear.is_none() && a.mic_missing => {
                if let Ok(name) = a.restart_ear() {
                    a.chat(Role::System, format!("麦克风已恢复：{name}"));
                    a.go_idle();
                }
            }
        }
    }
    a.tasks.cancel_all();
}

impl Assistant {
    fn emit(&self, ev: UiEvent) {
        (self.ui)(ev);
    }

    fn chat(&self, role: Role, text: impl Into<String>) {
        self.emit(UiEvent::Chat(role, text.into()));
    }

    async fn text_command(&mut self, text: &str, echo: bool) {
        self.set_mode(Mode::Muted);
        if self.handle(text, echo).await && !self.turn.cancelled() {
            self.classic_conversation(true).await; // 播报被唤醒词打断，接着听
        }
        self.go_idle();
    }

    fn set_mode(&self, mode: Mode) {
        audio::set_mode(mode);
    }

    fn mode(&self) -> Mode {
        audio::mode()
    }

    fn go_idle(&mut self) {
        self.ducker.set(false);
        self.phone = false;
        audio::phone_end_turn();
        while self.audio_rx.try_recv().is_ok() {}
        self.set_mode(if self.listening { Mode::Idle } else { Mode::Paused });
        self.emit(UiEvent::Partial(String::new()));
        self.emit(UiEvent::Status(if self.listening { Status::Idle } else { Status::Paused }));
    }

    /// 根据配置重建工具、前台助手和后台任务设置
    fn apply_config(&mut self) {
        let full_tools = Arc::new(Tools::new(&self.cfg, self.apps.clone()));
        self.front_tools = Arc::new(full_tools.with_tasks(self.tasks.clone(), self.reminders.clone()));
        self.tasks.update_settings(TaskSettings::from_config(&self.cfg, full_tools));
        self.agent = self.build_agent();
    }

    fn build_agent(&self) -> Option<Agent> {
        let key = config::llm_key(&self.cfg)?;
        let c = &self.cfg;
        match Llm::new(&c.llm_base_url, key, &c.llm_model, c.llm_thinking, &c.llm_effort) {
            Ok(llm) => Some(Agent::front(llm, self.front_tools.clone(), c.new_conversation_after_secs)),
            Err(e) => {
                self.chat(Role::System, format!("大模型初始化失败：{e}"));
                None
            }
        }
    }

    /// 重新打开麦克风，返回设备名
    fn restart_ear(&mut self) -> anyhow::Result<String> {
        self.ear = None; // 先释放旧的麦克风
        let settings = EarSettings {
            device: self.cfg.input_device.clone(),
            wake_words: self.cfg.wake_words.clone(),
            threshold: self.cfg.kws_threshold,
            score: self.cfg.kws_score,
            model_dir: config::models_dir().join("kws"),
        };
        let mode = if self.listening { Mode::Idle } else { Mode::Paused };
        match audio::start_ear(settings, mode, self.cmd_tx.clone(), self.audio_tx.clone()) {
            Ok((ear, name, problems)) => {
                self.ear = Some(ear);
                self.emit(UiEvent::Mic(name.clone()));
                for p in problems {
                    self.chat(Role::System, format!("唤醒词问题：{p}"));
                }
                Ok(name)
            }
            Err(e) => {
                self.mic_missing = audio::is_mic_unavailable(&e);
                self.emit(UiEvent::Mic("麦克风不可用".into()));
                Err(e)
            }
        }
    }

    fn reload(&mut self, new_cfg: Config) {
        let ear_changed = new_cfg.input_device != self.cfg.input_device
            || new_cfg.wake_words != self.cfg.wake_words
            || new_cfg.kws_threshold != self.cfg.kws_threshold
            || new_cfg.kws_score != self.cfg.kws_score;
        self.cfg = new_cfg;
        self.apply_config();
        if ear_changed {
            if let Err(e) = self.restart_ear() {
                self.chat(Role::System, format!("语音唤醒不可用：{e:#}"));
            }
        }
        self.go_idle();
    }

    fn realtime_enabled(&self) -> bool {
        self.cfg.voice_frontend == "realtime"
    }

    /// 一次语音对话。notice：后台任务完成时要先告诉用户的内容
    async fn conversation(&mut self, notice: Option<String>, origin: Origin) {
        // 结束时 go_idle 会恢复
        self.ducker.set(self.cfg.duck_others);
        if !matches!(origin, Origin::Wake | Origin::Phone) {
            // 只有唤醒词和手机触发的这轮，队列里才是用户刚说的话；其余情况都是残留
            while self.audio_rx.try_recv().is_ok() {}
        }
        if self.realtime_enabled() {
            match self.realtime_conversation(notice.as_deref(), origin).await {
                Ok(()) => {}
                // 用户点了停止：不提示错误、不降级、不再听
                Err(_) if self.turn.cancelled() => {}
                Err(e) => {
                    self.chat(Role::System, format!("{e:#}；这次改用“识别 + 大模型”。"));
                    if let Some(text) = notice {
                        self.speak(&text).await;
                    } else {
                        self.classic_conversation(true).await;
                    }
                }
            }
        } else {
            let mut barged = false;
            if let Some(text) = notice {
                self.chat(Role::Assistant, text.clone());
                barged = self.speak(&text).await;
                if !barged {
                    self.go_idle();
                    return;
                }
            }
            self.classic_conversation(!barged).await;
        }
        self.go_idle();
    }

    /// 后台任务做完了：主动告诉用户
    async fn announce(&mut self, done: TaskDone) {
        let text = done.message();
        self.conversation(Some(text), Origin::Notice).await;
    }

    // ---------- 识别 + 大模型 ----------

    /// 一轮对话；开启“继续聆听”或播报被打断时，接着听下一句
    async fn classic_conversation(&mut self, beep: bool) {
        let mut next = self.listen(beep).await;
        while let Some(text) = next.take() {
            let barged = self.handle(&text, true).await;
            if self.turn.cancelled() {
                break;
            }
            if barged || (self.cfg.follow_up && self.listening) {
                next = self.listen(barged).await;
            }
        }
    }

    /// 听一句话（云端流式识别）。没听到内容时返回 None。
    async fn listen(&mut self, beep: bool) -> Option<String> {
        if self.turn.cancelled() {
            return None;
        }
        if self.ear.is_none() && !self.phone {
            self.chat(Role::System, "麦克风不可用，可以在下方输入文字命令。");
            return None;
        }
        let Some(api_key) = config::asr_key(&self.cfg) else {
            self.chat(Role::System, "还没有设置语音识别的 API Key：请在“设置”页填写阿里云百炼的 key，点“保存并应用”。");
            return None;
        };
        self.set_mode(Mode::Capture);
        if beep && !self.phone {
            sys::beep(880, 120);
        }
        self.emit(UiEvent::Status(Status::Listening));
        let settings = AsrSettings {
            url: self.cfg.asr_url.clone(),
            model: self.cfg.asr_model.clone(),
            api_key,
            sentence_silence_ms: self.cfg.asr_sentence_silence_ms,
            speech_timeout: Duration::from_secs(self.cfg.asr_speech_timeout_secs),
        };
        let ui = self.ui.clone();
        let result = tokio::select! {
            r = asr::recognize(&settings, &mut self.audio_rx, |partial| ui(UiEvent::Partial(partial.to_string()))) => r,
            _ = self.turn.stopped() => Ok(None),
        };
        self.set_mode(Mode::Muted);
        while self.audio_rx.try_recv().is_ok() {}
        self.emit(UiEvent::Partial(String::new()));
        match result {
            Ok(text) => text,
            Err(e) => {
                self.chat(Role::System, format!("{e:#}"));
                None
            }
        }
    }

    /// 处理一句话并播报回复。返回播报是否被唤醒词打断
    async fn handle(&mut self, text: &str, echo: bool) -> bool {
        if echo {
            self.chat(Role::User, text);
        }
        self.emit(UiEvent::Status(Status::Thinking));
        // 直达规则里关闭程序要等它退出（最多几秒），不能占着异步线程干等
        let reply = if let Some(r) = tokio::task::block_in_place(|| fastpath::try_handle(text, &self.apps)) {
            r
        } else if let Some(agent) = self.agent.as_mut() {
            let ui = self.ui.clone();
            let on_tool = |name: &str, input: &Value| {
                ui(UiEvent::Status(Status::Acting));
                let text = describe_tool(name, input);
                if !text.is_empty() {
                    ui(UiEvent::Chat(Role::Tool, text));
                }
            };
            let asked = with_screen(text);
            let result = tokio::select! {
                r = agent.ask(&asked, on_tool) => Some(r),
                _ = self.turn.stopped() => None,
            };
            match result {
                Some(Ok(r)) => r,
                Some(Err(e)) => {
                    (self.ui)(UiEvent::Chat(Role::System, format!("{e:#}")));
                    format!("这次没能确认完成：{e}。")
                }
                None => {
                    // 中途停下时对话历史可能不完整（有工具调用没有结果），丢掉以免下一轮出错
                    agent.clear();
                    self.chat(Role::System, "（已停止）");
                    return false;
                }
            }
        } else {
            self.chat(Role::System, "还没有设置大模型的 API Key：请在“设置”页填写 DeepSeek 的 key，点“保存并应用”。");
            "还没有配置大模型。".into()
        };
        if self.turn.cancelled() {
            return false;
        }
        self.chat(Role::Assistant, reply.clone());
        self.speak(&reply).await
    }

    /// 系统播报。开启打断时播报期间仍听唤醒词，听到就停下并返回 true（这时已经在录音）
    async fn speak(&mut self, text: &str) -> bool {
        self.emit(UiEvent::Status(Status::Speaking));
        let barge = self.cfg.barge_in && self.ear.is_some() && self.listening;
        self.set_mode(if barge { Mode::Barge } else { Mode::Muted });
        let speaking = self.tts.speak(text, self.cfg.tts_rate);
        tokio::pin!(speaking);
        let notify = self.ear.as_ref().map(|e| e.barge.clone());
        loop {
            tokio::select! {
                _ = &mut speaking => break,
                _ = self.turn.stopped() => {
                    self.tts.stop();
                    (&mut speaking).await;
                    break;
                }
                _ = async { notify.as_ref().unwrap().notified().await }, if barge => {
                    if self.mode() == Mode::Capture {
                        self.tts.stop();
                        (&mut speaking).await;
                        self.chat(Role::System, "（已打断）");
                        return true;
                    }
                }
            }
        }
        self.set_mode(Mode::Muted);
        // 扬声器还有余音，多等一会儿再恢复收音，避免播报内容误触发唤醒
        tokio::time::sleep(Duration::from_millis(600)).await;
        false
    }

    // ---------- 实时语音 ----------

    async fn realtime_conversation(&mut self, notice: Option<&str>, origin: Origin) -> anyhow::Result<()> {
        if self.ear.is_none() && notice.is_none() && origin != Origin::Phone {
            anyhow::bail!("麦克风不可用");
        }
        let api_key = config::asr_key(&self.cfg).ok_or_else(|| anyhow::anyhow!("实时语音需要阿里云百炼的 API Key"))?;
        // 设置了自己的大模型（比如 DeepSeek）时，实时语音模型只负责听和说，判断、调工具都交给那个大模型；
        // 没设置时才让实时语音模型自己当大脑
        let local = self.agent.is_some();
        let settings = RealtimeSettings {
            url: self.cfg.realtime_url.clone(),
            model: self.cfg.realtime_model.clone(),
            voice: self.cfg.realtime_voice.clone(),
            api_key,
            // 唤醒时屏幕上在干什么；主界面在最前面时会跳过它，拿到用户刚才在看的窗口
            instructions: if local { VOICE_PROMPT.to_string() } else { format!("{FRONT_PROMPT}\n\n{}", screen::hint()) },
            tools: if local { Vec::new() } else { self.front_tools.definitions() },
            speech_rate: (1.0 + self.cfg.tts_rate as f32 * 0.1).clamp(0.5, 2.0),
        };
        self.emit(UiEvent::Status(Status::Thinking));
        let turn = self.turn;
        // 打开扬声器是同步阻塞的（最多几秒），放到阻塞线程里，期间也能响应停止；
        // 被停止时这个任务自己跑完，拿到的 Player 随任务结果一起释放
        let player = turn.guard(tokio::task::spawn_blocking(Player::open)).await;
        let mut player = match player {
            None => return Err(Cancelled.into()),
            Some(r) => r.map_err(|e| anyhow::anyhow!("打开扬声器失败：{e}"))??,
        };
        // 连接 + 配置确认整体有截止时间（Session::connect 内部有），期间可以停止
        let Some(session) = turn.guard(realtime::Session::connect(&settings)).await else {
            return Err(Cancelled.into());
        };
        let mut session = session?;
        // 唤醒后、连接好之前用户已经开口说的话：按顺序收起来（有上限），连好后先补发
        let mut preroll = audio::PreRoll::new(audio::PREROLL_MAX_SAMPLES);
        if matches!(origin, Origin::Wake | Origin::Phone) && self.mode() == Mode::Capture {
            while let Ok(chunk) = self.audio_rx.try_recv() {
                preroll.push(chunk);
            }
        } else {
            while self.audio_rx.try_recv().is_ok() {}
        }
        let mut thinking = Thinking::default();
        let result = self.realtime_loop(&mut session, &mut player, notice, local, &mut thinking, preroll).await;
        player.clear();
        self.stop_thinking(&mut thinking).await;
        // 关闭连接是尽力而为，别让它挡住回到待机
        let _ = tokio::time::timeout(Duration::from_secs(2), session.close()).await;
        result
    }

    /// 让自己的大模型在后台想这句话（不挡住收音和打断），想完由实时语音模型读出来
    fn think(&mut self, thinking: &mut Thinking, text: &str) {
        let Some(mut agent) = self.agent.take() else { return };
        let text = with_screen(text);
        let ui = self.ui.clone();
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        let job = tokio::spawn(async move {
            let on_tool = |name: &str, input: &Value| {
                ui(UiEvent::Status(Status::Acting));
                let text = describe_tool(name, input);
                if !text.is_empty() {
                    ui(UiEvent::Chat(Role::Tool, text));
                }
            };
            let result = tokio::select! {
                r = agent.ask(&text, on_tool) => Some(r),
                _ = cancel_rx => {
                    // 中途停下时对话历史可能不完整（有工具调用没有结果），丢掉以免下一轮出错
                    agent.clear();
                    None
                }
            };
            (agent, result)
        });
        thinking.job = Some((job, cancel_tx));
    }

    /// 停下还没想完的，把大模型收回来
    async fn stop_thinking(&mut self, thinking: &mut Thinking) {
        if let Some((job, cancel)) = thinking.job.take() {
            let _ = cancel.send(());
            if let Ok((agent, _)) = job.await {
                self.agent = Some(agent);
            }
        }
        if self.agent.is_none() {
            self.agent = self.build_agent();
        }
    }

    async fn realtime_loop(
        &mut self,
        session: &mut realtime::Session,
        player: &mut Player,
        notice: Option<&str>,
        local: bool,
        thinking: &mut Thinking,
        preroll: audio::PreRoll,
    ) -> anyhow::Result<()> {
        let turn = self.turn;
        // 已在 realtime_conversation 里检查过；这里再兜一次底，停止后绝不进入收音
        if turn.cancelled() {
            return Err(Cancelled.into());
        }
        let duplex = self.cfg.full_duplex;
        let barge = self.cfg.barge_in && self.listening;
        let notify = self.ear.as_ref().map(|e| e.barge.clone());
        let mut responding = false; // 模型正在生成（我们要的）回复
        let mut transcript = String::new();
        let mut calls: Vec<(String, String, String)> = Vec::new();
        let mut pending_notices: Vec<String> = Vec::new();
        // 自己的大模型当大脑时：排队等着想的话、想好了排队等着读的回复、当前这轮回复是谁要的
        let mut to_think: Vec<String> = Vec::new();
        let mut to_speak: Vec<String> = Vec::new();
        let mut speech = Speech::None;
        let mut ours_requested = false;
        let mut resume_at: Option<Instant> = None; // 播完后什么时候恢复收音
        let mut user_speaking = false;
        // “正在说”最长多久：服务端漏发说话结束时，不能让空闲退出永久失效
        let mut speaking_due: Option<Instant> = None;
        // 回复有时比用户那句话的转写先到；先压着，等转写到了再按顺序显示
        let mut awaiting_user_text = false;
        let mut held_reply: Option<String> = None;
        let mut last_activity = Instant::now();
        let mut wait_limit = Duration::from_secs(self.cfg.asr_speech_timeout_secs.max(3));
        let mut ticker = tokio::time::interval(Duration::from_millis(50));
        // 我们在等的那轮服务端回复：首个回复事件和后续进展各有截止时间
        let mut watch = ResponseWatch::default();
        // 用户说完（speech_stopped）后多久还没拿到转写就提示
        let mut transcript_due: Option<Instant> = None;

        if let Some(text) = notice {
            self.chat(Role::System, format!("（通知）{text}"));
            let text = format!("（这是系统通知，不是用户说的话）{text}。请用一两句话口语化地告诉用户这个结果。");
            if local {
                self.think(thinking, &text);
            } else {
                session.say(&text).await?;
                responding = true;
                watch.start(Instant::now());
            }
            self.set_mode(Mode::Muted);
        } else {
            self.set_mode(Mode::Capture);
            // 唤醒后已经说出口的部分先按顺序补发（realtime_conversation 已经把残留和本轮分开）
            for chunk in preroll.into_chunks() {
                // 补发期间也能停止；停止后不再提示音、不进入“正在听”
                match turn.guard(session.send_audio(&chunk)).await {
                    None => return Err(Cancelled.into()),
                    Some(r) => r?,
                }
            }
            if turn.cancelled() {
                return Err(Cancelled.into());
            }
            if !self.phone {
                sys::beep(880, 120);
            }
            self.emit(UiEvent::Status(Status::Listening));
        }

        loop {
            tokio::select! {
                _ = turn.stopped() => {
                    player.clear();
                    self.set_mode(Mode::Muted);
                    if responding {
                        session.cancel_response().await; // 内部最多等 1 秒
                    }
                    break;
                }
                chunk = self.audio_rx.recv() => {
                    if let Some(chunk) = chunk {
                        if self.mode() == Mode::Capture {
                            session.send_audio(&chunk).await?;
                        }
                    }
                }
                event = session.next_event() => {
                    last_activity = Instant::now();
                    let event = event?;
                    if matches!(event, realtime::Event::ResponseCreated | realtime::Event::Transcript(_) | realtime::Event::Audio(_) | realtime::Event::FunctionCall { .. }) {
                        watch.progress(Instant::now());
                    }
                    let output = matches!(event, realtime::Event::Transcript(_) | realtime::Event::Audio(_) | realtime::Event::FunctionCall { .. });
                    if local && output && speech != Speech::Ours {
                        // 服务端没发 response.created 时，按第一段输出来分：是我们要读的就留下，否则取消
                        if speech == Speech::None && std::mem::take(&mut ours_requested) {
                            speech = Speech::Ours;
                        } else {
                            if speech == Speech::None {
                                speech = Speech::Auto;
                                session.cancel_response().await;
                            }
                            continue;
                        }
                    }
                    match event {
                        realtime::Event::SpeechStarted => {
                            // 用户插话：服务端已经取消回复，本地也别再播了
                            user_speaking = true;
                            speaking_due = Some(Instant::now() + SPEAKING_MAX);
                            player.clear();
                            self.emit(UiEvent::Status(Status::Listening));
                        }
                        realtime::Event::SpeechStopped => {
                            user_speaking = false;
                            speaking_due = None;
                            awaiting_user_text = true;
                            transcript_due = Some(Instant::now() + TRANSCRIPT_DEADLINE);
                            if !local {
                                responding = true;
                                watch.start(Instant::now());
                            }
                            self.emit(UiEvent::Status(Status::Thinking));
                        }
                        realtime::Event::SpeechInvalid => {
                            // 服务端判定刚才不算一句话：不会有回复，回到等待说话
                            user_speaking = false;
                            speaking_due = None;
                            last_activity = Instant::now();
                            self.emit(UiEvent::Partial(String::new()));
                        }
                        realtime::Event::UserPartial(text) => self.emit(UiEvent::Partial(text)),
                        realtime::Event::UserText(text) => {
                            self.emit(UiEvent::Partial(String::new()));
                            awaiting_user_text = false;
                            transcript_due = None;
                            if !text.trim().is_empty() {
                                self.chat(Role::User, text.clone());
                                if local {
                                    // 正在想上一句时先排队，想完接着想这句
                                    if thinking.job.is_some() {
                                        to_think.push(text);
                                    } else {
                                        self.think(thinking, &text);
                                    }
                                }
                            }
                            if let Some(reply) = held_reply.take() {
                                self.chat(Role::Assistant, reply);
                            }
                        }
                        realtime::Event::ResponseCreated if local => {
                            // 服务端在用户说完话后会自己开始回答；只留我们要它读的那轮，其余的马上取消
                            speech = if std::mem::take(&mut ours_requested) { Speech::Ours } else { Speech::Auto };
                            if speech == Speech::Auto {
                                session.cancel_response().await;
                            }
                        }
                        realtime::Event::Transcript(delta) => transcript.push_str(&delta),
                        realtime::Event::Audio(bytes) => {
                            if !player.is_playing() {
                                self.emit(UiEvent::Status(Status::Speaking));
                                if !duplex {
                                    // 外放时不把麦克风发过去，否则模型会听到自己的声音；改为只听唤醒词
                                    self.set_mode(if barge { Mode::Barge } else { Mode::Muted });
                                }
                            }
                            player.push_pcm16(&bytes);
                            resume_at = None;
                        }
                        realtime::Event::FunctionCall { call_id, name, arguments } => calls.push((call_id, name, arguments)),
                        realtime::Event::ResponseDone(status) if local => {
                            let was = std::mem::replace(&mut speech, Speech::None);
                            transcript.clear(); // 显示的是大模型想好的原文，不用实时模型的转写
                            if was == Speech::Ours {
                                responding = false;
                                watch.finish();
                                if status == "failed" {
                                    self.chat(Role::System, "实时语音这一轮播报失败了");
                                }
                            }
                            if !responding && !to_speak.is_empty() {
                                let reply = to_speak.remove(0);
                                session.read_aloud(&reply).await?;
                                ours_requested = true;
                                responding = true;
                                watch.start(Instant::now());
                            }
                        }
                        realtime::Event::ResponseDone(status) => {
                            responding = false;
                            watch.finish();
                            if !transcript.trim().is_empty() {
                                let reply = std::mem::take(&mut transcript);
                                if awaiting_user_text {
                                    held_reply = Some(reply);
                                } else {
                                    self.chat(Role::Assistant, reply);
                                }
                            }
                            transcript.clear();
                            if status == "failed" {
                                self.chat(Role::System, "实时语音这一轮回复失败了");
                            }
                            if !calls.is_empty() {
                                // 执行工具，把结果交回去让模型接着说
                                self.emit(UiEvent::Status(Status::Acting));
                                for (call_id, name, arguments) in std::mem::take(&mut calls) {
                                    let input: Value = serde_json::from_str(&arguments).unwrap_or_default();
                                    let text = describe_tool(&name, &input);
                                    if !text.is_empty() {
                                        self.chat(Role::Tool, text);
                                    }
                                    let (output, _) = self.front_tools.call(&name, &input).await;
                                    // 网页结构可能很长，实时模型的上下文有限，截短一些
                                    let output: String = output.chars().take(8000).collect();
                                    session.send_function_output(&call_id, &output).await?;
                                }
                                responding = true;
                                // 工具可能跑了很久，从交回结果时重新计时
                                watch.start(Instant::now());
                            } else if let Some(text) = pending_notices.pop() {
                                session.say(&text).await?;
                                responding = true;
                                watch.start(Instant::now());
                            }
                        }
                        realtime::Event::Error(message) => {
                            self.chat(Role::System, format!("实时语音请求出错：{message}"));
                            // 正在等的回复还一点没出来就报错，多半不会再来了：结束等待，别一直“思考中”
                            if responding && !watch.seen_progress() {
                                responding = false;
                                watch.finish();
                                speech = Speech::None;
                                ours_requested = false;
                                self.emit(UiEvent::Status(Status::Listening));
                            }
                        }
                        realtime::Event::ResponseCreated | realtime::Event::Other => {}
                    }
                }
                finished = async { (&mut thinking.job.as_mut().unwrap().0).await }, if thinking.job.is_some() => {
                    thinking.job = None;
                    let reply = match finished {
                        Ok((agent, result)) => {
                            self.agent = Some(agent);
                            match result {
                                Some(Ok(reply)) => Some(reply),
                                Some(Err(e)) => {
                                    self.chat(Role::System, format!("{e:#}"));
                                    Some(format!("这次没能确认完成：{e}。"))
                                }
                                None => None,
                            }
                        }
                        Err(e) => {
                            self.agent = self.build_agent();
                            Some(format!("这次出错了：{e}。"))
                        }
                    };
                    if let Some(reply) = reply {
                        self.chat(Role::Assistant, reply.clone());
                        if responding || speech != Speech::None {
                            to_speak.push(reply);
                        } else {
                            session.read_aloud(&reply).await?;
                            ours_requested = true;
                            responding = true;
                            watch.start(Instant::now());
                        }
                    }
                    if !to_think.is_empty() {
                        let text = to_think.remove(0);
                        self.think(thinking, &text);
                    }
                }
                done = self.done_rx.recv() => {
                    if let Some(done) = done {
                        let text = done.message();
                        self.chat(Role::System, format!("（{}）{text}", if done.reminder { "提醒" } else { "后台任务通知" }));
                        let text = format!("（这是系统通知，不是用户说的话）{text}。请用一两句话口语化地告诉用户这个结果。");
                        if local {
                            if thinking.job.is_some() {
                                to_think.push(text);
                            } else {
                                self.think(thinking, &text);
                            }
                        } else if responding || user_speaking {
                            pending_notices.push(text);
                        } else {
                            session.say(&text).await?;
                            responding = true;
                            watch.start(Instant::now());
                        }
                    }
                }
                _ = async { notify.as_ref().unwrap().notified().await }, if barge && notify.is_some() => {
                    // 播报时说了唤醒词：停止播放、取消回复，开始听
                    if self.mode() == Mode::Capture {
                        player.clear();
                        if responding {
                            session.cancel_response().await;
                            responding = false;
                            watch.finish();
                        }
                        self.chat(Role::System, "（已打断）");
                        sys::beep(880, 80);
                        self.emit(UiEvent::Status(Status::Listening));
                        last_activity = Instant::now();
                        wait_limit = REALTIME_FOLLOW_UP;
                    }
                }
                _ = ticker.tick() => {
                    let now = Instant::now();
                    // 服务端回复卡住（连接还在但不出声、不结束）：有界结束这一轮并说明原因，可以再说一次
                    if let Some(reason) = watch.expired(now) {
                        player.clear();
                        session.cancel_response().await;
                        self.chat(Role::System, format!("实时语音{reason}，这一轮先结束了，可以再说一次。"));
                        break;
                    }
                    if speaking_due.is_some_and(|t| now >= t) {
                        speaking_due = None;
                        user_speaking = false;
                        last_activity = now;
                        self.emit(UiEvent::Partial(String::new()));
                        self.chat(Role::System, "很久没等到这句话说完，先不等了。");
                    }
                    if transcript_due.is_some_and(|t| now >= t) {
                        transcript_due = None;
                        awaiting_user_text = false;
                        if let Some(reply) = held_reply.take() {
                            self.chat(Role::Assistant, reply);
                        }
                        self.chat(Role::System, "没拿到刚才那句话的识别文字。");
                    }
                    let playing = player.is_playing();
                    // 播放中、自己的大模型还在想或还有回复没读：都是正常进展，不算空闲。
                    // 服务端回复（responding）不在这里续命，它由上面的截止时间管
                    let busy = thinking.job.is_some() || !to_think.is_empty() || !to_speak.is_empty();
                    if playing || busy {
                        last_activity = now;
                    }
                    let responding = responding || busy;
                    // 播完了：等余音过去再恢复收音
                    if !playing && !responding && self.mode() != Mode::Capture {
                        let at = *resume_at.get_or_insert_with(|| Instant::now() + ECHO_TAIL);
                        if Instant::now() >= at {
                            resume_at = None;
                            if !self.cfg.follow_up && notice.is_none() {
                                break;
                            }
                            while self.audio_rx.try_recv().is_ok() {}
                            self.set_mode(Mode::Capture);
                            self.emit(UiEvent::Status(Status::Listening));
                            last_activity = Instant::now();
                            wait_limit = REALTIME_FOLLOW_UP;
                        }
                    }
                    if !playing && !responding && !user_speaking && self.mode() == Mode::Capture && last_activity.elapsed() > wait_limit {
                        break; // 没人说话了，回到待机
                    }
                }
            }
        }
        if let Some(reply) = held_reply {
            self.chat(Role::Assistant, reply);
        }
        Ok(())
    }
}

/// 这一轮是怎么开始的：决定唤醒后录下的音频能不能当成用户的话
#[derive(Clone, Copy, PartialEq, Debug)]
enum Origin {
    /// 唤醒词：耳朵已经在录音，队列里是唤醒后说的话
    Wake,
    /// 点了“说话”
    Manual,
    /// 手机按住说话：队列里是手机传来的、用户刚说的话
    Phone,
    /// 后台任务完成通知
    Notice,
}

/// 用户点了停止（不是出错，不提示、不降级）
#[derive(Debug)]
struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("已停止")
    }
}

impl std::error::Error for Cancelled {}

/// 请求回复后多久没有任何回复事件（开始、文字、语音、工具调用）就放弃
const RESPONSE_FIRST_DEADLINE: Duration = Duration::from_secs(20);
/// 回复开始后多久没有新进展就放弃；持续出声、出文字的长回复会一直续上，不会误杀
const RESPONSE_STALL_DEADLINE: Duration = Duration::from_secs(30);
/// 用户一句话最长说多久（只在服务端漏发说话结束时起作用）
const SPEAKING_MAX: Duration = Duration::from_secs(60);
/// 用户说完后多久还没拿到识别文字
const TRANSCRIPT_DEADLINE: Duration = Duration::from_secs(15);

/// 等服务端回复的看门狗：只由真正的回复事件续期，不靠空闲计时器刷新
#[derive(Default)]
struct ResponseWatch {
    /// (开始等待的时刻, 最近一次进展的时刻)
    state: Option<(Instant, Option<Instant>)>,
}

impl ResponseWatch {
    fn start(&mut self, now: Instant) {
        self.state = Some((now, None));
    }

    fn progress(&mut self, now: Instant) {
        if let Some((_, last)) = &mut self.state {
            *last = Some(now);
        }
    }

    fn finish(&mut self) {
        self.state = None;
    }

    fn seen_progress(&self) -> bool {
        matches!(self.state, Some((_, Some(_))))
    }

    /// 超时时返回给用户看的原因
    fn expired(&self, now: Instant) -> Option<&'static str> {
        match self.state? {
            (since, None) if now.duration_since(since) > RESPONSE_FIRST_DEADLINE => Some("一直没有开始回复"),
            (_, Some(last)) if now.duration_since(last) > RESPONSE_STALL_DEADLINE => Some("回复中途卡住了"),
            _ => None,
        }
    }
}

/// 带上用户说话时屏幕上在干什么，“关闭当前直播”“复制这条帖子”才知道指哪个
fn with_screen(text: &str) -> String {
    let mut out = text.to_string();
    for extra in [crate::plugins::hint(text, &screen::exes()), screen::hint()] {
        if !extra.is_empty() {
            out.push_str("\n\n");
            out.push_str(&extra);
        }
    }
    out
}

/// 实时语音里正在后台想的大模型：任务拿着 Agent，想完连同结果一起还回来
#[derive(Default)]
struct Thinking {
    job: Option<(JoinHandle<(Agent, Option<anyhow::Result<String>>)>, oneshot::Sender<()>)>,
}

/// 实时语音服务端当前这一轮回复是谁要的
#[derive(Clone, Copy, PartialEq)]
enum Speech {
    None,
    /// 我们让它读的
    Ours,
    /// 用户说完话后它自己开始的回答（大脑是自己的大模型时要取消）
    Auto,
}

/// 工具调用在对话里显示成一句人话；返回空字符串表示不单独显示（后台任务自己会报告）
pub fn describe_tool(name: &str, input: &Value) -> String {
    let arg = |key: &str| -> String {
        let v = match &input[key] {
            Value::String(s) => s.trim().to_string(),
            Value::Null => String::new(),
            other => other.to_string(),
        };
        let short: String = v.chars().take(40).collect();
        if short.chars().count() < v.chars().count() { format!("{short}…") } else { short }
    };
    let with = |verb: &str, value: String| if value.is_empty() { verb.to_string() } else { format!("{verb}：{value}") };
    match name {
        "start_task" | "resume_task" | "cancel_task" => String::new(),
        "task_status" => "查看后台任务进度".into(),
        "launch_app" => with("打开应用", arg("name")),
        "find_apps" => with("查找应用", arg("keyword")),
        "web_search" => with("搜索", arg("query")),
        "open_url" | "browser_navigate" => with("打开网页", arg("url")),
        "current_time" => "查看时间".into(),
        "media_control" => match input["action"].as_str().unwrap_or("") {
            "volume_up" => "调大音量".into(),
            "volume_down" => "调小音量".into(),
            "mute" => "静音".into(),
            "play_pause" => "播放 / 暂停".into(),
            "next" => "下一首".into(),
            "previous" => "上一首".into(),
            _ => "控制媒体播放".into(),
        },
        "close_app" => with("关闭", arg("name")),
        "list_processes" => with("查看正在运行的程序", arg("keyword")),
        "desktop_windows" => "查看打开的窗口".into(),
        "desktop_focus" => with("切换到窗口", arg("name")),
        "desktop_snapshot" => with("读取窗口内容", arg("window")),
        "screen_read" => "看看屏幕上的内容".into(),
        "file_rename" => with("改名为", arg("new_name")),
        "file_find" => with("查找文件", arg("keyword")),
        "file_open" => with("打开文件", arg("path")),
        "file_move" => with(if input["copy"].as_bool() == Some(true) { "复制到" } else { "移动到" }, arg("to")),
        "file_delete" => with("删除到回收站", arg("path")),
        "file_info" => with("查看文件信息", arg("path")),
        "volume_set" => if input["mute"].as_bool() == Some(true) { "静音".into() } else { with("音量设为", input["percent"].as_u64().map(|p| format!("{p}%")).unwrap_or_default()) },
        "system_setting" => with("系统设置", arg("setting")),
        "screenshot" => "截图".into(),
        "power" => with("电源", arg("action")),
        "window_arrange" => with("排列窗口", arg("action")),
        "system_info" => "查看电脑状态".into(),
        "calc" => with("计算", arg("expression")),
        "clipboard_history" => "查看剪贴板历史".into(),
        "reminder_add" => with("设提醒", arg("text")),
        "reminder_list" => "查看提醒".into(),
        "plugin_guide" => with("读插件说明", arg("id")),
        "local_http" => with("调用本机接口", input["url"].as_str().unwrap_or("").rsplit('/').next().unwrap_or("").to_string()),
        "plugin_market" => match input["action"].as_str() { Some("install") => with("安装插件", arg("id")), Some("remove") => with("卸载插件", arg("id")), _ => "查看插件市场".into() },
        "history_search" => match input["query"].as_str().or(input["date"].as_str()) { Some(q) => with("查历史记录", q.to_string()), None => "查历史记录".into() },
        "reminder_cancel" => "取消提醒".into(),
        "app_update" => if input["action"] == "install" { "更新 Aeri".into() } else { "检查更新".into() },
        "clipboard_write" => "复制到剪贴板".into(),
        "clipboard_read" => "读取剪贴板".into(),
        "desktop_click" => if input["double"].as_bool() == Some(true) { "双击".into() } else { "点击".into() },
        "desktop_set_value" => with("填写", arg("value")),
        "desktop_select" => with("选择", arg("option")),
        "desktop_check" => match input["checked"].as_bool() {
            Some(true) => "勾选".into(),
            Some(false) => "取消勾选".into(),
            None => "切换开关".into(),
        },
        "desktop_scroll" => "滚动".into(),
        "desktop_type" | "browser_type" => with("输入", arg("text")),
        "desktop_keys" | "browser_press_key" => with("按键", arg(if name == "desktop_keys" { "keys" } else { "key" })),
        "browser_navigate_back" => "网页后退".into(),
        "browser_snapshot" => "读取网页内容".into(),
        "browser_click" => with("点击网页", arg("element")),
        "browser_hover" => with("指向", arg("element")),
        "browser_fill_form" => "填写表单".into(),
        "browser_select_option" => with("选择", arg("element")),
        "browser_find" => with("在网页里查找", arg("text")),
        "browser_tabs" => "切换浏览器标签页".into(),
        "browser_wait_for" => "等待网页加载".into(),
        "browser_handle_dialog" => "处理网页弹窗".into(),
        "browser_close" => "关闭浏览器".into(),
        _ => {
            let args: String = input.to_string().chars().take(60).collect();
            format!("{name} {args}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn signal() -> &'static StopSignal {
        Box::leak(Box::new(StopSignal::new()))
    }

    #[tokio::test]
    async fn stop_during_preparation_is_not_lost() {
        // 停止发生时没有任何地方在等（正在同步准备设备），之后再等也必须立刻看到
        let sig = signal();
        let turn = sig.turn();
        sig.stop();
        assert!(turn.cancelled());
        tokio::time::timeout(Duration::from_millis(200), turn.stopped()).await.expect("停止状态应持久");
    }

    #[tokio::test]
    async fn stop_does_not_leak_into_next_turn() {
        let sig = signal();
        let old = sig.turn();
        sig.stop();
        let next = sig.turn();
        assert!(old.cancelled());
        assert!(!next.cancelled());
        // 下一轮的等待不会被上一轮的停止立刻唤醒（notify_one 存许可就会）
        assert!(tokio::time::timeout(Duration::from_millis(100), next.stopped()).await.is_err());
    }

    #[tokio::test]
    async fn guard_aborts_slow_connect_and_blocks_fallback() {
        let sig = signal();
        let turn = sig.turn();
        let stopper = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            sig.stop();
        });
        // 模拟一直连不上的握手
        let started = Instant::now();
        let r = turn.guard(std::future::pending::<anyhow::Result<()>>()).await;
        stopper.await.unwrap();
        assert!(r.is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
        // 已停止的轮次里，后续的准备步骤（比如降级去听）直接不跑
        let ran = turn.guard(async { true }).await;
        assert_eq!(ran, None);
        // 新的一轮正常
        assert_eq!(sig.turn().guard(async { 7 }).await, Some(7));
    }

    #[tokio::test]
    async fn guard_drops_result_if_stopped_while_finishing() {
        let sig = signal();
        let turn = sig.turn();
        let r = turn
            .guard(async {
                sig.stop(); // 准备工作刚完成的同时点了停止
                1
            })
            .await;
        assert_eq!(r, None);
    }

    #[test]
    fn response_watch_bounds_missing_first_event_and_stall() {
        let t0 = Instant::now();
        let mut w = ResponseWatch::default();
        assert_eq!(w.expired(t0 + Duration::from_secs(3600)), None, "没在等回复时不计时");
        w.start(t0);
        assert_eq!(w.expired(t0 + RESPONSE_FIRST_DEADLINE - Duration::from_secs(1)), None);
        assert!(w.expired(t0 + RESPONSE_FIRST_DEADLINE + Duration::from_secs(1)).is_some());
        // 有进展后按卡住门限算；持续进展的长回复不会被误杀
        let mut t = t0;
        for _ in 0..20 {
            t += Duration::from_secs(10);
            w.progress(t);
            assert_eq!(w.expired(t + Duration::from_secs(5)), None);
        }
        assert!(w.seen_progress());
        assert!(w.expired(t + RESPONSE_STALL_DEADLINE + Duration::from_secs(1)).is_some());
        w.finish();
        assert_eq!(w.expired(t + Duration::from_secs(3600)), None);
    }

    #[test]
    fn progress_before_start_does_not_arm_watch() {
        let t0 = Instant::now();
        let mut w = ResponseWatch::default();
        w.progress(t0);
        assert!(!w.seen_progress());
        assert_eq!(w.expired(t0 + Duration::from_secs(3600)), None);
    }

    #[test]
    fn cancelled_texts_are_stale_new_ones_are_not() {
        let g = text_generation();
        assert!(!text_is_stale(g));
        cancel_pending_texts();
        assert!(text_is_stale(g));
        assert!(!text_is_stale(text_generation()));
    }

    #[test]
    fn tool_descriptions_are_readable() {
        assert_eq!(describe_tool("launch_app", &json!({"name": "计算器"})), "打开应用：计算器");
        assert_eq!(describe_tool("media_control", &json!({"action": "volume_up"})), "调大音量");
        assert_eq!(describe_tool("browser_snapshot", &json!({})), "读取网页内容");
        // 后台任务自己会报告，不重复显示
        assert_eq!(describe_tool("start_task", &json!({"task": "整理桌面"})), "");
        let long = describe_tool("desktop_type", &json!({"text": "字".repeat(60)}));
        assert!(long.ends_with('…') && long.chars().count() < 50);
    }
}
