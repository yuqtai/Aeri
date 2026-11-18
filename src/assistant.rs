//! 调度中心：前台负责对话，复杂任务交给后台智能体（Codex / Claude Code / 内置助手），做完主动播报。
//! 前台有两种：
//! - 实时语音（默认）：唤醒后麦克风音频直接发给百炼实时语音模型，边说边回，可以插话；
//! - 识别 + 大模型：fun-asr 识别一句话 → 本地直达规则 / DeepSeek → 系统播报。
//! 两种都支持播报时说唤醒词打断。

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
/// 本地大模型模式下，SpeechStopped 后最多等百炼自发回复多久；超时未收到则直接放行待朗读内容
const AUTO_TURN_MAX_WAIT: Duration = Duration::from_millis(1500);

fn missing_asr_key_msg() -> &'static str {
    crate::locale::pick(
        "Speech recognition needs an API key for the selected provider. Add it in Settings and select Save.",
        "还没有设置所选语音识别服务的 API Key：请在“设置”页填写后点“保存并应用”。",
    )
}

fn missing_voice_key_msg() -> &'static str {
    crate::locale::pick(
        "Voice conversation needs an API key for the selected provider. Add it in Settings and select Save.",
        "还没有设置所选语音服务的 API Key：请在“设置 → 语音对话”填写并保存后即可语音对话。",
    )
}

enum DeferredCmd {
    Cmd(Command),
    /// 收到 Command::Text 时已立刻回显过用户气泡，轮到执行时不再重复回显
    EchoedText { text: String, generation: u64 },
}

struct Assistant {
    cfg: Config,
    ui: UiSink,
    cmd_tx: UnboundedSender<Command>,
    cmd_rx: UnboundedReceiver<Command>,
    deferred_cmds: VecDeque<DeferredCmd>,
    pending_reload: Option<Config>,
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

pub async fn run(cfg: Config, rx: UnboundedReceiver<Command>, cmd_tx: UnboundedSender<Command>, ui: UiSink) {
    let apps = Arc::new(AppIndex::load_cached());
    {
        let apps = apps.clone();
        tokio::task::spawn_blocking(move || apps.refresh());
    }
    let (audio_tx, audio_rx) = unbounded_channel();
    audio::set_phone_output(audio_tx.clone());
    let (done_tx, done_rx) = unbounded_channel();
    let reminders = Reminders::new(done_tx.clone());
    crate::notify::serve(done_tx.clone());
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
        cmd_rx: rx,
        deferred_cmds: VecDeque::new(),
        pending_reload: None,
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
        a.chat(Role::System, crate::locale::pick(&format!("Voice wake-up unavailable: {e:#}"), &format!("语音唤醒不可用：{e:#}")).to_string());
    }
    a.go_idle();
    let mut mic_retry = tokio::time::interval(MIC_RETRY);

    loop {
        a.apply_pending_reload();
        if let Some(deferred) = a.deferred_cmds.pop_front() {
            a.run_deferred(deferred).await;
            continue;
        }
        tokio::select! {
            cmd = a.cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                a.run_command(cmd).await;
            }
            done = a.done_rx.recv() => {
                if let Some(done) = done {
                    a.turn = STOP.turn();
                    a.announce(done).await;
                }
            }
            _ = mic_retry.tick(), if a.ear.is_none() && a.mic_missing => {
                if let Ok(name) = a.restart_ear() {
                    a.chat(Role::System, crate::locale::pick(&format!("Microphone restored: {name}"), &format!("麦克风已恢复：{name}")).to_string());
                    a.go_idle();
                }
            }
        }
    }
    a.tasks.cancel_all();
}

impl Assistant {
    async fn run_deferred(&mut self, deferred: DeferredCmd) {
        match deferred {
            DeferredCmd::Cmd(cmd) => self.run_command(cmd).await,
            DeferredCmd::EchoedText { text, generation } => {
                self.turn = STOP.turn();
                if !text_is_stale(generation) {
                    self.text_command(&text, false).await;
                }
            }
        }
    }

    async fn run_command(&mut self, cmd: Command) {
        self.turn = STOP.turn();
        match cmd {
            Command::Wake(_word) => {
                // 耳朵检测到唤醒词时会立刻切到录音模式；如果此后模式被改过（比如期间在处理别的命令），这个唤醒已过时
                let fresh = self.ear.is_some() && self.mode() == Mode::Capture;
                if self.listening && fresh {
                    #[cfg(debug_assertions)]
                    println!("[唤醒] {_word}");
                    self.conversation(None, Origin::Wake).await;
                } else if self.mode() == Mode::Capture {
                    // 耳朵已切到录音但这次唤醒不算数：恢复模式并丢掉已录下的，免得堆积或串入下一轮
                    self.go_idle();
                }
            }
            Command::Talk => self.conversation(None, Origin::Manual).await,
            Command::PhoneTalk => {
                // 手机按一下就松开了（没存下声音）时不用开始
                if audio::phone_begin_turn() {
                    self.phone = true;
                    self.conversation(None, Origin::Phone).await;
                } else {
                    self.go_idle();
                }
            }
            Command::Text(text) => self.text_command(&text, true).await,
            Command::TextSubmitted { text, generation } => {
                self.emit(UiEvent::TextStarted(generation));
                if !text_is_stale(generation) {
                    self.text_command(&text, false).await;
                }
            }
            Command::SetListening(on) => {
                self.listening = on;
                self.go_idle();
            }
            Command::MicChanged => {
                let was_ok = self.ear.is_some();
                self.ear = None; // 先释放旧的
                match self.restart_ear() {
                    Ok(name) if !was_ok => self.chat(Role::System, crate::locale::pick(&format!("Microphone restored: {name}"), &format!("麦克风已恢复：{name}")).to_string()),
                    Ok(_) => {}
                    Err(_) => self.chat(Role::System, crate::locale::pick("Microphone disconnected. It will recover when reconnected; you can type for now.", "麦克风断开了，重新接上后会自动恢复；现在可以打字使用。")),
                }
                self.go_idle();
            }
            Command::ClearHistory => {
                if let Some(agent) = self.agent.as_mut() {
                    agent.clear();
                }
            }
            Command::Reload(new_cfg) => self.reload(new_cfg),
        }
    }

    fn apply_pending_reload(&mut self) {
        if let Some(new_cfg) = self.pending_reload.take() {
            self.reload(new_cfg);
        }
    }

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
        self.apply_pending_reload();
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
        match Llm::new(&c.llm_provider, &c.llm_base_url, key, &c.llm_model, c.llm_thinking, &c.llm_effort) {
            Ok(llm) => Some(Agent::front(llm, self.front_tools.clone(), c.new_conversation_after_secs)),
            Err(e) => {
                self.chat(Role::System, crate::locale::pick(&format!("Could not initialize the language model: {e}"), &format!("大模型初始化失败：{e}")).to_string());
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
                self.mic_missing = false;
                self.ear = Some(ear);
                self.emit(UiEvent::Mic(name.clone()));
                for p in problems {
                    self.chat(Role::System, crate::locale::pick(&format!("Wake-word problem: {p}"), &format!("唤醒词问题：{p}")).to_string());
                }
                Ok(name)
            }
            Err(e) => {
                self.mic_missing = audio::is_mic_unavailable(&e);
                self.emit(UiEvent::Mic(crate::locale::pick("Microphone unavailable", "麦克风不可用").into()));
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
                self.chat(Role::System, crate::locale::pick(&format!("Voice wake-up unavailable: {e:#}"), &format!("语音唤醒不可用：{e:#}")).to_string());
            }
        }
        self.go_idle();
    }

    fn realtime_enabled(&self) -> bool {
        self.cfg.voice_frontend == "realtime"
    }

    /// 一次语音对话。notice：后台任务完成时要先告诉用户的内容
    async fn conversation(&mut self, notice: Option<String>, origin: Origin) {
        if !matches!(origin, Origin::Wake | Origin::Phone) {
            // 只有唤醒词和手机触发的这轮，队列里才是用户刚说的话；其余情况都是残留
            while self.audio_rx.try_recv().is_ok() {}
        }
        if notice.is_none() {
            if self.ear.is_none() && origin != Origin::Phone {
                self.chat(Role::System, crate::locale::pick("Microphone unavailable. You can type a command below.", "麦克风不可用，可以在下方输入文字命令。"));
                self.go_idle();
                self.apply_pending_reload();
                return;
            }
            if !self.realtime_enabled() && config::asr_key(&self.cfg).is_none() {
                self.chat(Role::System, missing_asr_key_msg());
                self.go_idle();
                self.apply_pending_reload();
                return;
            }
            if self.realtime_enabled() && config::realtime_key(&self.cfg).is_none() && config::asr_key(&self.cfg).is_none() {
                self.chat(Role::System, missing_voice_key_msg());
                self.go_idle();
                self.apply_pending_reload();
                return;
            }
        }
        // 结束时 go_idle 会恢复
        self.ducker.set(self.cfg.duck_others);
        if self.realtime_enabled() {
            if notice.is_some() && config::realtime_key(&self.cfg).is_none() {
                if let Some(text) = notice {
                    self.chat(Role::Assistant, text.clone());
                    let _ = self.speak(&text).await;
                }
            } else {
                match self.realtime_conversation(notice.as_deref(), origin).await {
                    Ok(()) => {}
                    // 用户点了停止：不提示错误、不降级、不再听
                    Err(_) if self.turn.cancelled() => {}
                    Err(e) if config::asr_key(&self.cfg).is_none() => {
                        if let Some(text) = notice {
                            self.chat(Role::Assistant, text.clone());
                            let _ = self.speak(&text).await;
                        } else {
                            self.chat(Role::System, format!("{e:#}"));
                        }
                    }
                    Err(_) if self.ear.is_none() && notice.is_none() && origin != Origin::Phone => {
                        self.chat(Role::System, crate::locale::pick("Microphone unavailable. You can type a command below.", "麦克风不可用，可以在下方输入文字命令。"));
                    }
                    Err(e) => {
                        self.chat(
                            Role::System,
                            crate::locale::pick(
                                &format!("{e:#}; using speech recognition and the language model for this turn."),
                                &format!("{e:#}；这次改用“识别 + 大模型”。"),
                            )
                            .to_string(),
                        );
                        if let Some(text) = notice {
                            self.chat(Role::Assistant, text.clone());
                            let _ = self.speak(&text).await;
                        } else {
                            self.classic_conversation(true).await;
                        }
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
                    self.apply_pending_reload();
                    return;
                }
            }
            self.classic_conversation(!barged).await;
        }
        self.go_idle();
        self.apply_pending_reload();
    }

    /// 提醒或后台任务做完了：主动告诉用户；已暂停唤醒或全屏时保持安静只发文字
    async fn announce(&mut self, done: TaskDone) {
        let text = done.message();
        if should_silence_notice(self.listening, screen::front_is_fullscreen()) {
            self.chat(Role::Assistant, text);
            return;
        }
        if done.reminder {
            sys::beep(988, 180);
        }
        self.conversation(Some(text), Origin::Notice).await;
    }

    // ---------- 识别 + 大模型 ----------

    /// 处理思考/播报期间积压在通道里的非阻塞控制命令
    fn drain_commands_between_turns(&mut self) {
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            let _ = self.on_listen_command(cmd, true);
        }
    }

    /// listen 等待期间收到控制命令：返回 true 表示立即结束本次收音等待
    fn on_listen_command(&mut self, cmd: Command, idle: bool) -> bool {
        match cmd {
            Command::MicChanged => {
                let was_capture = self.mode() == Mode::Capture;
                let was_ok = self.ear.is_some();
                self.ear = None;
                match self.restart_ear() {
                    Ok(name) => {
                        if !was_ok {
                            self.chat(
                                Role::System,
                                crate::locale::pick(&format!("Microphone restored: {name}"), &format!("麦克风已恢复：{name}")).to_string(),
                            );
                        }
                        if was_capture {
                            self.set_mode(Mode::Capture);
                        }
                        false
                    }
                    Err(_) => {
                        if was_capture {
                            self.set_mode(Mode::Capture);
                        }
                        if !self.phone && !audio::phone_active() {
                            self.chat(
                                Role::System,
                                crate::locale::pick(
                                    "Microphone disconnected. It will recover when reconnected; you can type for now.",
                                    "麦克风断开了，重新接上后会自动恢复；现在可以打字使用。",
                                ),
                            );
                            true
                        } else {
                            false
                        }
                    }
                }
            }
            Command::SetListening(on) => {
                self.listening = on;
                !on && idle && !self.phone && !audio::phone_active()
            }
            Command::ClearHistory => {
                if let Some(a) = self.agent.as_mut() {
                    a.clear();
                }
                false
            }
            Command::Reload(new_cfg) => {
                self.pending_reload = Some(new_cfg);
                idle
            }
            Command::Text(text) => {
                self.chat(Role::User, text.clone());
                self.deferred_cmds.push_back(DeferredCmd::EchoedText { text, generation: text_generation() });
                idle
            }
            Command::TextSubmitted { text, generation } => {
                self.deferred_cmds.push_back(DeferredCmd::Cmd(Command::TextSubmitted { text, generation }));
                idle
            }
            Command::PhoneTalk => {
                self.deferred_cmds.push_back(DeferredCmd::Cmd(Command::PhoneTalk));
                idle && !self.phone
            }
            Command::Talk => {
                if self.phone {
                    self.deferred_cmds.push_back(DeferredCmd::Cmd(Command::Talk));
                    idle
                } else {
                    false
                }
            }
            Command::Wake(_) => false,
        }
    }

    /// 一轮对话；开启“继续聆听”或播报被打断时，接着听下一句
    async fn classic_conversation(&mut self, beep: bool) {
        let mut next = self.listen(beep).await;
        while let Some(text) = next.take() {
            let barged = self.handle(&text, true).await;
            if self.turn.cancelled() {
                break;
            }
            self.drain_commands_between_turns();
            let has_pending = self.pending_reload.is_some() || !self.deferred_cmds.is_empty();
            if barged
                || should_continue_classic_followup(
                    self.cfg.follow_up,
                    self.listening,
                    self.phone || audio::phone_active(),
                    self.ear.is_some(),
                    has_pending,
                )
            {
                next = self.listen(barged).await;
            }
        }
    }

    /// 听一句话（流式或离线识别）。没听到内容时返回 None。
    async fn listen(&mut self, beep: bool) -> Option<String> {
        if self.turn.cancelled() {
            return None;
        }
        if self.ear.is_none() && !self.phone {
            self.chat(Role::System, crate::locale::pick("Microphone unavailable. You can type a command below.", "麦克风不可用，可以在下方输入文字命令。"));
            return None;
        }
        let Some(api_key) = config::asr_key(&self.cfg) else {
            self.chat(Role::System, missing_asr_key_msg());
            return None;
        };
        let settings = AsrSettings {
            provider: asr::Provider::from_id(&self.cfg.asr_provider),
            region: config::azure_region(&self.cfg),
            local_model: self.cfg.local_asr_model.clone(),
            url: self.cfg.asr_url.clone(),
            model: self.cfg.asr_model.clone(),
            api_key,
            sentence_silence_ms: self.cfg.asr_sentence_silence_ms,
            speech_timeout: Duration::from_secs(self.cfg.asr_speech_timeout_secs),
        };
        if settings.provider == asr::Provider::Local {
            let ui = self.ui.clone();
            let prepared = tokio::select! {
                result = asr::prepare_local(&self.cfg.local_asr_model, |status| ui(UiEvent::Chat(Role::System, status.into()))) => Some(result),
                _ = self.turn.stopped() => None,
            };
            match prepared {
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    self.chat(Role::System, format!("{e:#}"));
                    return None;
                }
                None => return None,
            }
        }
        if beep && !self.phone {
            // 先保存唤醒词后已录下的音频，静音播完提示音后再放回队列并切回录音，防止把提示音录进去
            let mut saved = Vec::new();
            if self.mode() == Mode::Capture {
                while let Ok(chunk) = self.audio_rx.try_recv() {
                    saved.push(chunk);
                }
            }
            self.set_mode(Mode::Muted);
            if self.turn.guard(tokio::task::spawn_blocking(|| sys::beep_sync(880, 100))).await.is_none() {
                return None;
            }
            while self.audio_rx.try_recv().is_ok() {}
            for chunk in saved {
                let _ = self.audio_tx.send(chunk);
            }
        }
        self.set_mode(Mode::Capture);
        self.emit(UiEvent::Status(Status::Listening));
        let mut audio_rx = std::mem::replace(&mut self.audio_rx, unbounded_channel().1);
        let turn = self.turn;
        let ui = self.ui.clone();
        let heard_any = AtomicBool::new(false);
        let result = {
            let recognize_fut = asr::recognize(&settings, &mut audio_rx, |partial| {
                if !partial.is_empty() {
                    heard_any.store(true, Ordering::SeqCst);
                }
                ui(UiEvent::Partial(partial.to_string()));
            });
            tokio::pin!(recognize_fut);
            loop {
                tokio::select! {
                    r = &mut recognize_fut => break r,
                    _ = turn.stopped() => break Ok(None),
                    cmd = self.cmd_rx.recv() => {
                        let Some(cmd) = cmd else { break Ok(None) };
                        let idle = !heard_any.load(Ordering::SeqCst) && !audio::phone_is_held();
                        if self.on_listen_command(cmd, idle) {
                            break Ok(None);
                        }
                    }
                }
            }
        };
        self.set_mode(Mode::Muted);
        while audio_rx.try_recv().is_ok() {}
        self.audio_rx = audio_rx;
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
        let reply = if let Some(r) = run_fastpath(text, &self.apps) {
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
                    crate::locale::pick(&format!("I could not confirm completion: {e}."), &format!("这次没能确认完成：{e}。")).to_string()
                }
                None => {
                    // 中途停下时对话历史可能不完整（有工具调用没有结果），丢掉以免下一轮出错
                    agent.clear();
                    self.chat(Role::System, crate::locale::pick("(Stopped)", "（已停止）"));
                    return false;
                }
            }
        } else {
            self.chat(Role::System, crate::locale::pick("No language model API key is configured. Enter your provider key in Settings and select Save and apply.", "还没有设置大模型的 API Key：请在“设置”页填写 DeepSeek 的 key，点“保存并应用”。"));
            crate::locale::pick("No language model is configured yet.", "还没有配置大模型。").into()
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
        let barge = (self.cfg.barge_in || self.cfg.talk_barge) && self.ear.is_some() && self.listening;
        crate::audio::set_talk_barge(self.cfg.talk_barge);
        self.set_mode(if barge { Mode::Barge } else { Mode::Muted });
        let ui = self.ui.clone();
        let speaking = self.tts.speak_configured(&self.cfg, text, |note| ui(UiEvent::Chat(Role::System, note.into())));
        tokio::pin!(speaking);
        let notify = self.ear.as_ref().map(|e| e.barge.clone());
        let mut stopped = false;
        loop {
            tokio::select! {
                _ = &mut speaking => break,
                _ = self.turn.stopped() => {
                    self.tts.stop();
                    (&mut speaking).await;
                    stopped = true;
                    break;
                }
                _ = async { notify.as_ref().unwrap().notified().await }, if barge => {
                    if self.mode() == Mode::Capture {
                        self.tts.stop();
                        (&mut speaking).await;
                        self.chat(Role::System, crate::locale::pick("(Interrupted)", "（已打断）"));
                        return true;
                    }
                }
            }
        }
        self.set_mode(Mode::Muted);
        if !stopped && !self.turn.cancelled() {
            // 扬声器还有余音，多等一会儿再恢复收音，避免播报内容误触发唤醒；被停止时跳过等待立即返回
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(600)) => {}
                _ = self.turn.stopped() => {}
            }
        }
        false
    }

    // ---------- 实时语音 ----------

    async fn realtime_conversation(&mut self, notice: Option<&str>, origin: Origin) -> anyhow::Result<()> {
        if self.ear.is_none() && notice.is_none() && origin != Origin::Phone {
            anyhow::bail!(crate::locale::pick("Microphone unavailable", "麦克风不可用"));
        }
        let api_key = config::realtime_key(&self.cfg).ok_or_else(|| anyhow::anyhow!(crate::locale::pick("Realtime voice needs an API key for the selected provider", "实时语音需要所选服务的 API Key")))?;
        // 设置了自己的大模型（比如 DeepSeek）时，实时语音模型只负责听和说，判断、调工具都交给那个大模型；
        // 没设置时才让实时语音模型自己当大脑
        let local = self.agent.is_some();
        let settings = RealtimeSettings {
            provider: realtime::Provider::from_id(&self.cfg.realtime_provider),
            listen_only: local,
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
            Some(r) => r.map_err(|e| anyhow::anyhow!(crate::locale::pick(&format!("Could not open the speaker: {e}"), &format!("打开扬声器失败：{e}")).to_string()))??,
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
        let server_auto_turns = local && settings.provider == realtime::Provider::DashScope;
        let result = self
            .realtime_loop(&mut session, &mut player, notice, origin, local, server_auto_turns, &mut thinking, preroll)
            .await;
        player.clear();
        self.stop_thinking(&mut thinking).await;
        // 关闭连接是尽力而为，别让它挡住回到待机
        let _ = tokio::time::timeout(Duration::from_secs(2), session.close()).await;
        result
    }

    /// 本地大模型模式下处理一句用户识别文字：优先走本地极速直达规则，未命中再交给后台思考
    #[allow(clippy::too_many_arguments)]
    async fn dispatch_local_text(
        &mut self,
        user_text: String,
        session: &mut realtime::Session,
        thinking: &mut Thinking,
        to_think: &mut Vec<String>,
        to_speak: &mut Vec<String>,
        responding: &mut bool,
        speech: Speech,
        auto_turn_active: bool,
        ours_requested: &mut bool,
        watch: &mut ResponseWatch,
    ) -> anyhow::Result<()> {
        self.chat(Role::User, user_text.clone());
        let fast = if thinking.job.is_none() && to_think.is_empty() {
            run_fastpath(&user_text, &self.apps)
        } else {
            None
        };
        if let Some(reply) = fast {
            self.chat(Role::Assistant, reply.clone());
            if *responding || speech != Speech::None || auto_turn_active {
                to_speak.push(reply);
            } else {
                session.read_aloud(&reply).await?;
                *ours_requested = true;
                *responding = true;
                watch.start(Instant::now());
            }
        } else if thinking.job.is_some() {
            to_think.push(user_text);
        } else {
            self.think(thinking, &user_text);
        }
        Ok(())
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

    #[allow(clippy::too_many_arguments)]
    async fn realtime_loop(
        &mut self,
        session: &mut realtime::Session,
        player: &mut Player,
        notice: Option<&str>,
        origin: Origin,
        local: bool,
        server_auto_turns: bool,
        thinking: &mut Thinking,
        mut preroll: audio::PreRoll,
    ) -> anyhow::Result<()> {
        let turn = self.turn;
        // 已在 realtime_conversation 里检查过；这里再兜一次底，停止后绝不进入收音
        if turn.cancelled() {
            return Err(Cancelled.into());
        }
        let duplex = self.cfg.full_duplex;
        let mut barge = (self.cfg.barge_in || self.cfg.talk_barge) && self.listening;
        crate::audio::set_talk_barge(self.cfg.talk_barge);
        let mut notify = self.ear.as_ref().map(|e| e.barge.clone());
        let mut responding = false; // 模型正在生成（我们要的）回复
        let mut transcript = String::new();
        let mut calls: Vec<(String, String, String)> = Vec::new();
        let mut pending_notices: Vec<String> = Vec::new();
        // 自己的大模型当大脑时：排队等着想的话、想好了排队等着读的回复、当前这轮回复是谁要的
        let mut to_think: Vec<String> = Vec::new();
        let mut to_speak: Vec<String> = Vec::new();
        let mut speech = Speech::None;
        let mut ours_requested = false;
        let mut auto_turn_active = false;
        let mut auto_turn_deadline: Option<Instant> = None;
        let mut phone_buffer = String::new();
        let mut clear_agent_on_return = false;
        let mut end_after_reply = false;
        let mut completed_turn = false;
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
            self.chat(Role::System, crate::locale::pick(&format!("(Notification) {text}"), &format!("（通知）{text}")).to_string());
            let text = format!("(System notification, not a user message) {text}. Tell the user this result in one or two conversational sentences. Reply in the language the user speaks.");
            if local {
                self.think(thinking, &text);
            } else {
                session.say(&text).await?;
                responding = true;
                watch.start(Instant::now());
            }
            self.set_mode(Mode::Muted);
        } else {
            if matches!(origin, Origin::Wake | Origin::Phone) && self.mode() == Mode::Capture {
                while let Ok(chunk) = self.audio_rx.try_recv() {
                    preroll.push(chunk);
                }
            }
            if !self.phone {
                // 在静音态下同步播完提示音再开麦，避免把蜂鸣声录进去（唤醒预录音频保留在 preroll 中）
                self.set_mode(Mode::Muted);
                if turn.guard(tokio::task::spawn_blocking(|| sys::beep_sync(880, 100))).await.is_none() {
                    return Err(Cancelled.into());
                }
                while self.audio_rx.try_recv().is_ok() {}
            }
            // 唤醒后已经说出口的部分先按顺序补发（realtime_conversation 已经把残留和本轮分开）
            for chunk in preroll.into_chunks() {
                // 补发期间也能停止；停止后不再进入“正在听”
                match turn.guard(session.send_audio(&chunk)).await {
                    None => return Err(Cancelled.into()),
                    Some(r) => r?,
                }
            }
            if turn.cancelled() {
                return Err(Cancelled.into());
            }
            self.set_mode(Mode::Capture);
            self.emit(UiEvent::Status(Status::Listening));
            last_activity = Instant::now();
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
                        if speech == Speech::None && !auto_turn_active && std::mem::take(&mut ours_requested) {
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
                            // 用户插话：服务端已经取消回复，本地也立即停下旧思考、清空待读和旧工具调用
                            user_speaking = true;
                            completed_turn = false;
                            speaking_due = Some(Instant::now() + SPEAKING_MAX);
                            player.clear();
                            self.stop_thinking(thinking).await;
                            to_think.clear();
                            to_speak.clear();
                            calls.clear();
                            transcript.clear();
                            speech = Speech::None;
                            ours_requested = false;
                            auto_turn_active = false;
                            auto_turn_deadline = None;
                            responding = false;
                            watch.finish();
                            self.emit(UiEvent::Status(Status::Listening));
                        }
                        realtime::Event::SpeechStopped => {
                            user_speaking = false;
                            speaking_due = None;
                            awaiting_user_text = true;
                            transcript_due = Some(Instant::now() + TRANSCRIPT_DEADLINE);
                            if server_auto_turns {
                                auto_turn_active = true;
                                auto_turn_deadline = Some(Instant::now() + AUTO_TURN_MAX_WAIT);
                            } else if !local {
                                responding = true;
                                watch.start(Instant::now());
                            }
                            if !((origin == Origin::Phone || audio::phone_active()) && audio::phone_is_held()) {
                                self.emit(UiEvent::Status(Status::Thinking));
                            }
                        }
                        realtime::Event::SpeechInvalid => {
                            // 服务端判定刚才不算一句话：不会有自发回复，回到等待说话
                            user_speaking = false;
                            speaking_due = None;
                            auto_turn_active = false;
                            auto_turn_deadline = None;
                            last_activity = Instant::now();
                            if phone_buffer.is_empty() {
                                self.emit(UiEvent::Partial(String::new()));
                            }
                            if local && !responding && speech == Speech::None && !to_speak.is_empty() {
                                let reply = to_speak.remove(0);
                                session.read_aloud(&reply).await?;
                                ours_requested = true;
                                responding = true;
                                watch.start(Instant::now());
                            }
                        }
                        realtime::Event::UserPartial(part) => {
                            if local && (origin == Origin::Phone || audio::phone_active()) && !phone_buffer.is_empty() {
                                self.emit(UiEvent::Partial(format!("{phone_buffer}{part}")));
                            } else {
                                self.emit(UiEvent::Partial(part));
                            }
                        }
                        realtime::Event::UserText(text) => {
                            awaiting_user_text = false;
                            transcript_due = None;
                            if local && (origin == Origin::Phone || audio::phone_active()) {
                                let piece = text.trim();
                                if !piece.is_empty() {
                                    if !phone_buffer.is_empty() && piece.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()) {
                                        phone_buffer.push(' ');
                                    }
                                    phone_buffer.push_str(piece);
                                }
                                if audio::phone_is_held() {
                                    self.emit(UiEvent::Partial(phone_buffer.clone()));
                                } else {
                                    self.emit(UiEvent::Partial(String::new()));
                                    let full = std::mem::take(&mut phone_buffer);
                                    if !full.trim().is_empty() {
                                        self.dispatch_local_text(
                                            full,
                                            session,
                                            thinking,
                                            &mut to_think,
                                            &mut to_speak,
                                            &mut responding,
                                            speech,
                                            auto_turn_active,
                                            &mut ours_requested,
                                            &mut watch,
                                        )
                                        .await?;
                                    } else if self.mode() == Mode::Capture && !player.is_playing() && !responding && thinking.job.is_none() {
                                        self.emit(UiEvent::Status(Status::Listening));
                                    }
                                }
                            } else {
                                self.emit(UiEvent::Partial(String::new()));
                                if !text.trim().is_empty() {
                                    if local {
                                        self.dispatch_local_text(
                                            text,
                                            session,
                                            thinking,
                                            &mut to_think,
                                            &mut to_speak,
                                            &mut responding,
                                            speech,
                                            auto_turn_active,
                                            &mut ours_requested,
                                            &mut watch,
                                        )
                                        .await?;
                                    } else {
                                        self.chat(Role::User, text);
                                    }
                                } else if local && self.mode() == Mode::Capture && !player.is_playing() && !responding && thinking.job.is_none() {
                                    self.emit(UiEvent::Status(Status::Listening));
                                }
                                if let Some(reply) = held_reply.take() {
                                    self.chat(Role::Assistant, reply);
                                }
                            }
                        }
                        realtime::Event::ResponseCreated if local => {
                            // 服务端在用户说完话后会自己开始回答；只留我们要它读的那轮，其余的马上取消
                            speech = if !auto_turn_active && std::mem::take(&mut ours_requested) {
                                Speech::Ours
                            } else {
                                Speech::Auto
                            };
                            if speech == Speech::Auto {
                                session.cancel_response().await;
                            }
                        }
                        realtime::Event::Transcript(delta) => {
                            transcript.push_str(&delta);
                            if !local && !awaiting_user_text && !user_speaking {
                                self.emit(UiEvent::Partial(transcript.clone()));
                            }
                        }
                        realtime::Event::Audio(bytes) => {
                            if !player.is_playing() {
                                self.emit(UiEvent::Status(Status::Speaking));
                                if !duplex {
                                    // 外放时不把麦克风发过去，否则模型会听到自己的声音；改为只听唤醒词或直接说话打断
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
                                completed_turn = true;
                                if status == "failed" {
                                    self.chat(Role::System, crate::locale::pick("Realtime voice playback failed for this turn.", "实时语音这一轮播报失败了"));
                                }
                            } else {
                                auto_turn_active = false;
                                auto_turn_deadline = None;
                            }
                            if !responding && speech == Speech::None && !auto_turn_active && !to_speak.is_empty() {
                                let reply = to_speak.remove(0);
                                session.read_aloud(&reply).await?;
                                ours_requested = true;
                                responding = true;
                                completed_turn = false;
                                watch.start(Instant::now());
                            }
                        }
                        realtime::Event::ResponseDone(status) => {
                            responding = false;
                            watch.finish();
                            if !user_speaking {
                                self.emit(UiEvent::Partial(String::new()));
                            }
                            if status == "cancelled" {
                                // 已被打断的回复：绝不执行半截工具调用，清空残留
                                calls.clear();
                                transcript.clear();
                                continue;
                            }
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
                                self.chat(Role::System, crate::locale::pick("Realtime voice response failed for this turn.", "实时语音这一轮回复失败了"));
                            }
                            if !calls.is_empty() {
                                // 执行工具，把结果交回去让模型接着说
                                self.emit(UiEvent::Status(Status::Acting));
                                for (call_id, name, arguments) in std::mem::take(&mut calls) {
                                    if turn.cancelled() {
                                        break;
                                    }
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
                                if !turn.cancelled() {
                                    responding = true;
                                    // 工具可能跑了很久，从交回结果时重新计时
                                    watch.start(Instant::now());
                                }
                            } else if let Some(text) = pending_notices.pop() {
                                session.say(&text).await?;
                                responding = true;
                                watch.start(Instant::now());
                            } else {
                                completed_turn = true;
                            }
                        }
                        realtime::Event::Error(message) => {
                            self.chat(Role::System, crate::locale::pick(&format!("Realtime voice request failed: {message}"), &format!("实时语音请求出错：{message}")).to_string());
                            // 正在等的回复还一点没出来就报错，多半不会再来了：结束等待，别一直“思考中”
                            if responding && !watch.seen_progress() {
                                responding = false;
                                watch.finish();
                                speech = Speech::None;
                                ours_requested = false;
                                auto_turn_active = false;
                                auto_turn_deadline = None;
                                self.emit(UiEvent::Status(Status::Listening));
                            }
                        }
                        realtime::Event::ResponseCreated | realtime::Event::Other => {}
                    }
                }
                finished = async { (&mut thinking.job.as_mut().unwrap().0).await }, if thinking.job.is_some() => {
                    thinking.job = None;
                    let reply = match finished {
                        Ok((mut agent, result)) => {
                            if std::mem::take(&mut clear_agent_on_return) {
                                agent.clear();
                            }
                            self.agent = Some(agent);
                            match result {
                                Some(Ok(reply)) => Some(reply),
                                Some(Err(e)) => {
                                    self.chat(Role::System, format!("{e:#}"));
                                    Some(crate::locale::pick(&format!("I could not confirm completion: {e}."), &format!("这次没能确认完成：{e}。")).to_string())
                                }
                                None => None,
                            }
                        }
                        Err(e) => {
                            clear_agent_on_return = false;
                            self.agent = self.build_agent();
                            Some(crate::locale::pick(&format!("An error occurred: {e}."), &format!("这次出错了：{e}。")).to_string())
                        }
                    };
                    if let Some(reply) = reply {
                        self.chat(Role::Assistant, reply.clone());
                        if responding || speech != Speech::None || auto_turn_active {
                            to_speak.push(reply);
                        } else {
                            session.read_aloud(&reply).await?;
                            ours_requested = true;
                            responding = true;
                            completed_turn = false;
                            watch.start(Instant::now());
                        }
                    }
                    while !to_think.is_empty() && thinking.job.is_none() {
                        let text = to_think.remove(0);
                        if let Some(reply) = run_fastpath(&text, &self.apps) {
                            self.chat(Role::Assistant, reply.clone());
                            if responding || speech != Speech::None || auto_turn_active {
                                to_speak.push(reply);
                            } else {
                                session.read_aloud(&reply).await?;
                                ours_requested = true;
                                responding = true;
                                completed_turn = false;
                                watch.start(Instant::now());
                            }
                        } else {
                            self.think(thinking, &text);
                        }
                    }
                }
                done = self.done_rx.recv() => {
                    if let Some(done) = done {
                        let text = done.message();
                        if should_silence_notice(self.listening, screen::front_is_fullscreen()) {
                            self.chat(Role::Assistant, text);
                        } else {
                            if done.reminder {
                                sys::beep(988, 180);
                            }
                            let tag = if done.reminder {
                                crate::locale::pick("Reminder", "提醒")
                            } else if done.notice {
                                crate::locale::pick("Notification", "通知")
                            } else {
                                crate::locale::pick("Background task notification", "后台任务通知")
                            };
                            self.chat(
                                Role::System,
                                crate::locale::pick(&format!("({tag}) {text}"), &format!("（{tag}）{text}")).to_string(),
                            );
                            let text = format!("(System notification, not a user message) {text}. Tell the user this result in one or two conversational sentences. Reply in the language the user speaks.");
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
                                completed_turn = false;
                                watch.start(Instant::now());
                            }
                        }
                    }
                }
                cmd = self.cmd_rx.recv() => {
                    let Some(cmd) = cmd else { break };
                    let playing = player.is_playing();
                    let phone_holding = (origin == Origin::Phone || audio::phone_active())
                        && (audio::phone_is_held() || !phone_buffer.is_empty());
                    let busy = thinking.job.is_some()
                        || !to_think.is_empty()
                        || !to_speak.is_empty()
                        || !calls.is_empty()
                        || !pending_notices.is_empty();
                    let idle_waiting = !playing
                        && !responding
                        && !busy
                        && speech != Speech::Ours
                        && !user_speaking
                        && !awaiting_user_text
                        && !phone_holding;
                    match cmd {
                        Command::MicChanged => {
                            let was_capture = self.mode() == Mode::Capture;
                            let was_barge = self.mode() == Mode::Barge;
                            let was_ok = self.ear.is_some();
                            self.ear = None;
                            match self.restart_ear() {
                                Ok(name) => {
                                    notify = self.ear.as_ref().map(|e| e.barge.clone());
                                    if !was_ok {
                                        self.chat(
                                            Role::System,
                                            crate::locale::pick(&format!("Microphone restored: {name}"), &format!("麦克风已恢复：{name}")).to_string(),
                                        );
                                    }
                                    if was_capture {
                                        self.set_mode(Mode::Capture);
                                    } else if was_barge && barge {
                                        self.set_mode(Mode::Barge);
                                    } else {
                                        self.set_mode(Mode::Muted);
                                    }
                                }
                                Err(_) => {
                                    notify = None;
                                    self.set_mode(if was_capture { Mode::Capture } else { Mode::Muted });
                                    if !self.phone && !audio::phone_active() {
                                        self.chat(
                                            Role::System,
                                            crate::locale::pick(
                                                "Microphone disconnected. It will recover when reconnected; you can type for now.",
                                                "麦克风断开了，重新接上后会自动恢复；现在可以打字使用。",
                                            ),
                                        );
                                        if idle_waiting || user_speaking {
                                            if responding {
                                                session.cancel_response().await;
                                            }
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        Command::SetListening(on) => {
                            self.listening = on;
                            barge = (self.cfg.barge_in || self.cfg.talk_barge) && self.listening;
                            if !on {
                                if self.mode() == Mode::Barge {
                                    self.set_mode(Mode::Muted);
                                }
                                if idle_waiting {
                                    break;
                                }
                            }
                        }
                        Command::ClearHistory => {
                            if let Some(a) = self.agent.as_mut() {
                                a.clear();
                            } else {
                                clear_agent_on_return = true;
                            }
                            if !local {
                                if idle_waiting {
                                    break;
                                } else {
                                    end_after_reply = true;
                                }
                            }
                        }
                        Command::Reload(new_cfg) => {
                            self.pending_reload = Some(new_cfg);
                            if idle_waiting {
                                break;
                            }
                        }
                        Command::Text(text) => {
                            self.chat(Role::User, text.clone());
                            self.deferred_cmds.push_back(DeferredCmd::EchoedText {
                                text,
                                generation: text_generation(),
                            });
                            if idle_waiting {
                                break;
                            }
                        }
                        Command::TextSubmitted { text, generation } => {
                            self.deferred_cmds.push_back(DeferredCmd::Cmd(Command::TextSubmitted { text, generation }));
                            if idle_waiting {
                                break;
                            }
                        }
                        Command::PhoneTalk => {
                            self.deferred_cmds.push_back(DeferredCmd::Cmd(Command::PhoneTalk));
                            if idle_waiting {
                                break;
                            }
                        }
                        Command::Talk => {
                            if idle_waiting {
                                if origin == Origin::Phone || notice.is_some() {
                                    self.deferred_cmds.push_back(DeferredCmd::Cmd(Command::Talk));
                                    break;
                                }
                                resume_at = None;
                                completed_turn = false;
                                self.set_mode(Mode::Capture);
                                self.emit(UiEvent::Status(Status::Listening));
                                last_activity = Instant::now();
                                wait_limit = REALTIME_FOLLOW_UP;
                            } else {
                                self.deferred_cmds.push_back(DeferredCmd::Cmd(Command::Talk));
                            }
                        }
                        Command::Wake(_) => {}
                    }
                }
                _ = async { notify.as_ref().unwrap().notified().await }, if barge && notify.is_some() => {
                    // 播报时说了唤醒词或直接说话打断：停止播放、取消旧思考与回复，静音响完提示音后再开始听
                    if self.mode() == Mode::Capture {
                        let mut barge_preroll = Vec::new();
                        while let Ok(chunk) = self.audio_rx.try_recv() {
                            barge_preroll.push(chunk);
                        }
                        self.set_mode(Mode::Muted);
                        player.clear();
                        self.stop_thinking(thinking).await;
                        to_think.clear();
                        to_speak.clear();
                        calls.clear();
                        transcript.clear();
                        speech = Speech::None;
                        ours_requested = false;
                        auto_turn_active = false;
                        auto_turn_deadline = None;
                        completed_turn = false;
                        session.cancel_response().await;
                        responding = false;
                        watch.finish();
                        self.chat(Role::System, crate::locale::pick("(Interrupted)", "（已打断）"));
                        if !self.phone {
                            if turn.guard(tokio::task::spawn_blocking(|| sys::beep_sync(880, 100))).await.is_none() {
                                break;
                            }
                            while self.audio_rx.try_recv().is_ok() {}
                        }
                        for chunk in barge_preroll {
                            session.send_audio(&chunk).await?;
                        }
                        self.set_mode(Mode::Capture);
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
                        self.chat(Role::System, crate::locale::pick(&format!("Realtime voice {reason}. This turn has ended; please try again."), &format!("实时语音{reason}，这一轮先结束了，可以再说一次。")).to_string());
                        break;
                    }
                    // 兜底：SpeechStopped 后最多等 1.5s，若百炼未发自发回复则直接放行待朗读内容
                    if auto_turn_active && speech == Speech::None && auto_turn_deadline.is_some_and(|d| now >= d) {
                        auto_turn_active = false;
                        auto_turn_deadline = None;
                        if !responding && !to_speak.is_empty() {
                            let reply = to_speak.remove(0);
                            session.read_aloud(&reply).await?;
                            ours_requested = true;
                            responding = true;
                            completed_turn = false;
                            watch.start(Instant::now());
                        }
                    }
                    // 手机长按已松手，且没有后半句正在说或等待转写：将完整长句一次性送去处理
                    if local
                        && (origin == Origin::Phone || audio::phone_active())
                        && !audio::phone_is_held()
                        && !user_speaking
                        && !awaiting_user_text
                        && !phone_buffer.is_empty()
                    {
                        self.emit(UiEvent::Partial(String::new()));
                        let full = std::mem::take(&mut phone_buffer);
                        if !full.trim().is_empty() {
                            self.dispatch_local_text(
                                full,
                                session,
                                thinking,
                                &mut to_think,
                                &mut to_speak,
                                &mut responding,
                                speech,
                                auto_turn_active,
                                &mut ours_requested,
                                &mut watch,
                            )
                            .await?;
                        }
                    }
                    if speaking_due.is_some_and(|t| now >= t) {
                        speaking_due = None;
                        user_speaking = false;
                        last_activity = now;
                        self.emit(UiEvent::Partial(String::new()));
                        self.chat(Role::System, crate::locale::pick("Timed out waiting for the end of your sentence.", "很久没等到这句话说完，先不等了。"));
                    }
                    if transcript_due.is_some_and(|t| now >= t) {
                        transcript_due = None;
                        awaiting_user_text = false;
                        if let Some(reply) = held_reply.take() {
                            self.chat(Role::Assistant, reply);
                        }
                        self.chat(Role::System, crate::locale::pick("No transcript was received for that sentence.", "没拿到刚才那句话的识别文字。"));
                    }
                    let playing = player.is_playing();
                    let phone_holding = (origin == Origin::Phone || audio::phone_active())
                        && (audio::phone_is_held() || !phone_buffer.is_empty());
                    // 播放中、自己的大模型还在想、有回复待读或手机仍按着说话：都是正常进展，不算空闲。
                    // 服务端回复（responding）不在这里续命，它由上面的截止时间管
                    let busy = thinking.job.is_some() || !to_think.is_empty() || !to_speak.is_empty();
                    if playing || busy || phone_holding {
                        last_activity = now;
                    }
                    let active_responding = responding || busy;
                    if !playing
                        && !active_responding
                        && !user_speaking
                        && !phone_holding
                        && (!self.deferred_cmds.is_empty() || self.pending_reload.is_some() || end_after_reply)
                    {
                        break;
                    }
                    let can_follow_up = should_continue_realtime_followup(
                        notice.is_some(),
                        origin,
                        audio::phone_active(),
                        self.cfg.follow_up,
                        self.listening,
                        self.ear.is_some() || self.phone,
                    );
                    if completed_turn && !playing && !active_responding && !user_speaking && !phone_holding {
                        if !can_follow_up {
                            break;
                        }
                        if self.mode() == Mode::Capture {
                            completed_turn = false;
                            self.emit(UiEvent::Status(Status::Listening));
                            last_activity = Instant::now();
                            wait_limit = REALTIME_FOLLOW_UP;
                        }
                    }
                    // 播完了：允许追问（或手机仍按住说话）则等余音过去再恢复收音；不允许（通知、手机按住已松手、关闭继续聆听、暂停唤醒）立即结束
                    if !playing && !active_responding && self.mode() != Mode::Capture {
                        if !can_follow_up && !phone_holding {
                            break;
                        }
                        let at = *resume_at.get_or_insert_with(|| Instant::now() + ECHO_TAIL);
                        if Instant::now() >= at {
                            resume_at = None;
                            completed_turn = false;
                            if !phone_holding {
                                while self.audio_rx.try_recv().is_ok() {}
                            }
                            self.set_mode(Mode::Capture);
                            self.emit(UiEvent::Status(Status::Listening));
                            last_activity = Instant::now();
                            wait_limit = REALTIME_FOLLOW_UP;
                        }
                    }
                    if !playing
                        && !active_responding
                        && !user_speaking
                        && !phone_holding
                        && self.mode() == Mode::Capture
                        && last_activity.elapsed() > wait_limit
                    {
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
        f.write_str(crate::locale::pick("Stopped", "已停止"))
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
            (since, None) if now.duration_since(since) > RESPONSE_FIRST_DEADLINE => Some(crate::locale::pick("did not start responding", "一直没有开始回复")),
            (_, Some(last)) if now.duration_since(last) > RESPONSE_STALL_DEADLINE => Some(crate::locale::pick("stalled during the response", "回复中途卡住了")),
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

/// 在多线程运行时里放到阻塞线程池跑直达规则（关闭程序等可能短暂停留），在单线程测试运行时里直接跑
fn run_fastpath(text: &str, apps: &AppIndex) -> Option<String> {
    if tokio::runtime::Handle::try_current()
        .is_ok_and(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
    {
        tokio::task::block_in_place(|| fastpath::try_handle(text, apps))
    } else {
        fastpath::try_handle(text, apps)
    }
}

/// 已暂停唤醒或前台全屏时，通知与提醒保持静默（只写文字与历史，不压音、不外放）
fn should_silence_notice(listening: bool, fullscreen: bool) -> bool {
    !listening || fullscreen
}

/// 实时对话播报结束后是否允许自动开麦追问（提醒/后台通知、手机按住说话、暂停唤醒、无麦克风时一律禁止开麦）
fn should_continue_realtime_followup(
    has_notice: bool,
    origin: Origin,
    phone_active: bool,
    follow_up: bool,
    listening: bool,
    mic_ready: bool,
) -> bool {
    !has_notice && origin != Origin::Phone && !phone_active && follow_up && listening && mic_ready
}

/// 经典模式播报结束后是否允许进入下一轮继续聆听
fn should_continue_classic_followup(
    follow_up: bool,
    listening: bool,
    phone: bool,
    mic_ready: bool,
    has_pending_control: bool,
) -> bool {
    follow_up && listening && !phone && mic_ready && !has_pending_control
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
    let with = |verb: &str, value: String| if value.is_empty() { verb.to_string() } else { crate::locale::pick(&format!("{verb}: {value}"), &crate::locale::pick(&format!("{verb}: {value}"), &format!("{verb}：{value}")).to_string()).to_string() };
    match name {
        "start_task" | "resume_task" | "cancel_task" => String::new(),
        "task_status" => crate::locale::pick("Check background progress", "查看后台任务进度").into(),
        "launch_app" => with(crate::locale::pick("Open app", "打开应用"), arg("name")),
        "find_apps" => with(crate::locale::pick("Find app", "查找应用"), arg("keyword")),
        "web_search" => with(crate::locale::pick("Search", "搜索"), arg("query")),
        "open_url" | "browser_navigate" => with(crate::locale::pick("Open webpage", "打开网页"), arg("url")),
        "current_time" => crate::locale::pick("Check time", "查看时间").into(),
        "media_control" => match input["action"].as_str().unwrap_or("") {
            "volume_up" => crate::locale::pick("Increase volume", "调大音量").into(),
            "volume_down" => crate::locale::pick("Decrease volume", "调小音量").into(),
            "mute" => crate::locale::pick("Mute", "静音").into(),
            "play_pause" => crate::locale::pick("Play / pause", "播放 / 暂停").into(),
            "next" => crate::locale::pick("Next track", "下一首").into(),
            "previous" => crate::locale::pick("Previous track", "上一首").into(),
            _ => crate::locale::pick("Control media", "控制媒体播放").into(),
        },
        "close_app" => with(crate::locale::pick("Close", "关闭"), arg("name")),
        "list_processes" => with(crate::locale::pick("List running apps", "查看正在运行的程序"), arg("keyword")),
        "desktop_windows" => crate::locale::pick("List open windows", "查看打开的窗口").into(),
        "desktop_focus" => with(crate::locale::pick("Switch to window", "切换到窗口"), arg("name")),
        "desktop_snapshot" => with(crate::locale::pick("Read window", "读取窗口内容"), arg("window")),
        "screen_read" => crate::locale::pick("Read screen", "看看屏幕上的内容").into(),
        "file_rename" => with(crate::locale::pick("Rename to", "改名为"), arg("new_name")),
        "file_find" => with(crate::locale::pick("Find files", "查找文件"), arg("keyword")),
        "file_open" => with(crate::locale::pick("Open file", "打开文件"), arg("path")),
        "file_move" => with(if input["copy"].as_bool() == Some(true) { crate::locale::pick("Copy to", "复制到") } else { crate::locale::pick("Move to", "移动到") }, arg("to")),
        "file_delete" => with(crate::locale::pick("Move to Recycle Bin", "删除到回收站"), arg("path")),
        "file_info" => with(crate::locale::pick("View file information", "查看文件信息"), arg("path")),
        "volume_set" => if input["mute"].as_bool() == Some(true) { crate::locale::pick("Mute", "静音").into() } else { with(crate::locale::pick("Set volume to", "音量设为"), input["percent"].as_u64().map(|p| format!("{p}%")).unwrap_or_default()) },
        "system_setting" => with(crate::locale::pick("System settings", "系统设置"), arg("setting")),
        "screenshot" => crate::locale::pick("Take screenshot", "截图").into(),
        "power" => with(crate::locale::pick("Power", "电源"), arg("action")),
        "window_arrange" => with(crate::locale::pick("Arrange window", "排列窗口"), arg("action")),
        "system_info" => crate::locale::pick("Check computer status", "查看电脑状态").into(),
        "calc" => with(crate::locale::pick("Calculate", "计算"), arg("expression")),
        "clipboard_history" => crate::locale::pick("View clipboard history", "查看剪贴板历史").into(),
        "reminder_add" => with(crate::locale::pick("Set reminder", "设提醒"), arg("text")),
        "reminder_list" => crate::locale::pick("View reminders", "查看提醒").into(),
        "plugin_guide" => with(crate::locale::pick("Read plugin guide", "读插件说明"), arg("id")),
        "local_http" => with(crate::locale::pick("Call local API", "调用本机接口"), input["url"].as_str().unwrap_or("").rsplit('/').next().unwrap_or("").to_string()),
        "plugin_market" => match input["action"].as_str() { Some("install") => with(crate::locale::pick("Install plugin", "安装插件"), arg("id")), Some("remove") => with(crate::locale::pick("Remove plugin", "卸载插件"), arg("id")), _ => crate::locale::pick("View plugin market", "查看插件市场").into() },
        "history_search" => match input["query"].as_str().or(input["date"].as_str()) { Some(q) => with(crate::locale::pick("Search history", "查历史记录"), q.to_string()), None => crate::locale::pick("Search history", "查历史记录").into() },
        "reminder_cancel" => crate::locale::pick("Cancel reminder", "取消提醒").into(),
        "app_update" => if input["action"] == "install" { crate::locale::pick("Update Aeri", "更新 Aeri").into() } else { crate::locale::pick("Check for updates", "检查更新").into() },
        "clipboard_write" => crate::locale::pick("Copy to clipboard", "复制到剪贴板").into(),
        "clipboard_read" => crate::locale::pick("Read clipboard", "读取剪贴板").into(),
        "desktop_click" => if input["double"].as_bool() == Some(true) { crate::locale::pick("Double-click", "双击").into() } else { crate::locale::pick("Click", "点击").into() },
        "desktop_set_value" => with(crate::locale::pick("Fill", "填写"), arg("value")),
        "desktop_select" => with(crate::locale::pick("Select", "选择"), arg("option")),
        "desktop_check" => match input["checked"].as_bool() {
            Some(true) => crate::locale::pick("Check", "勾选").into(),
            Some(false) => crate::locale::pick("Uncheck", "取消勾选").into(),
            None => crate::locale::pick("Toggle switch", "切换开关").into(),
        },
        "desktop_scroll" => crate::locale::pick("Scroll", "滚动").into(),
        "desktop_type" | "browser_type" => with(crate::locale::pick("Type", "输入"), arg("text")),
        "desktop_keys" | "browser_press_key" => with(crate::locale::pick("Press key", "按键"), arg(if name == "desktop_keys" { "keys" } else { "key" })),
        "browser_navigate_back" => crate::locale::pick("Go back", "网页后退").into(),
        "browser_snapshot" => crate::locale::pick("Read webpage", "读取网页内容").into(),
        "browser_click" => with(crate::locale::pick("Click webpage", "点击网页"), arg("element")),
        "browser_hover" => with(crate::locale::pick("Hover", "指向"), arg("element")),
        "browser_fill_form" => crate::locale::pick("Fill form", "填写表单").into(),
        "browser_select_option" => with(crate::locale::pick("Select", "选择"), arg("element")),
        "browser_find" => with(crate::locale::pick("Find on page", "在网页里查找"), arg("text")),
        "browser_tabs" => crate::locale::pick("Switch browser tab", "切换浏览器标签页").into(),
        "browser_wait_for" => crate::locale::pick("Wait for page", "等待网页加载").into(),
        "browser_handle_dialog" => crate::locale::pick("Handle webpage dialog", "处理网页弹窗").into(),
        "browser_close" => crate::locale::pick("Close browser", "关闭浏览器").into(),
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
        assert_eq!(describe_tool("launch_app", &json!({"name": "计算器"})), crate::locale::pick("Open app: 计算器", "打开应用：计算器"));
        assert_eq!(describe_tool("media_control", &json!({"action": "volume_up"})), crate::locale::pick("Increase volume", "调大音量"));
        assert_eq!(describe_tool("browser_snapshot", &json!({})), crate::locale::pick("Read webpage", "读取网页内容"));
        // 后台任务自己会报告，不重复显示
        assert_eq!(describe_tool("start_task", &json!({"task": "整理桌面"})), "");
        let long = describe_tool("desktop_type", &json!({"text": "字".repeat(60)}));
        assert!(long.ends_with('…') && long.chars().count() < 50);
    }

    #[test]
    fn notice_phone_and_paused_never_auto_open_mic_for_followup() {
        // 已暂停唤醒或全屏时通知保持静默
        assert!(should_silence_notice(false, false));
        assert!(should_silence_notice(true, true));
        assert!(should_silence_notice(false, true));
        assert!(!should_silence_notice(true, false));

        // 正常唤醒或手动点击、开启继续聆听、未暂停唤醒且有麦克风时才允许追问
        assert!(should_continue_realtime_followup(false, Origin::Wake, false, true, true, true));
        assert!(should_continue_realtime_followup(false, Origin::Manual, false, true, true, true));

        // 提醒 / 后台任务通知播完绝不擅自开麦
        assert!(!should_continue_realtime_followup(true, Origin::Notice, false, true, true, true));
        // 手机按住说话回答完立即结束，不卡 8 秒压低音量
        assert!(!should_continue_realtime_followup(false, Origin::Phone, false, true, true, true));
        assert!(!should_continue_realtime_followup(false, Origin::Wake, true, true, true, true));
        // 关闭继续聆听、暂停唤醒或麦克风断开时绝不进入追问
        assert!(!should_continue_realtime_followup(false, Origin::Wake, false, false, true, true));
        assert!(!should_continue_realtime_followup(false, Origin::Wake, false, true, false, true));
        assert!(!should_continue_realtime_followup(false, Origin::Wake, false, true, true, false));

        // 经典模式同样遵守手机按住说话、暂停唤醒与待执行命令边界
        assert!(should_continue_classic_followup(true, true, false, true, false));
        assert!(!should_continue_classic_followup(true, true, true, true, false));
        assert!(!should_continue_classic_followup(true, false, false, true, false));
        assert!(!should_continue_classic_followup(true, true, false, false, false));
        assert!(!should_continue_classic_followup(true, true, false, true, true));
    }
}
