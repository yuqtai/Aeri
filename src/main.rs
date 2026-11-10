// 发布版不弹控制台窗口；调试版保留控制台方便看日志
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
mod asr;
mod assistant;
mod audio;
mod ball;
mod config;
mod duck;
mod fastpath;
mod kws;
mod llm;
mod mcp;
mod player;
mod realtime;
mod reminders;
mod remote;
mod history;
mod plugins;
mod resample;
mod screen;
mod sys;
mod tasks;
mod tools;
mod tts;
mod update;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use slint::{ComponentHandle, Model, ModelRc, VecModel};
use tokio::runtime::Handle;
use tokio::sync::mpsc::{self, UnboundedSender};

use assistant::{Command, UiEvent};
use config::Config;

slint::include_modules!();

const MAX_CHAT_ITEMS: usize = 200;
const EFFORTS: [&str; 3] = ["low", "high", "max"];
const EXECUTORS: [&str; 3] = ["codex", "claude", "builtin"];
const ACCESS: [&str; 3] = ["full", "workspace", "readonly"];
const FRONTENDS: [&str; 2] = ["realtime", "classic"];

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--kws-test") {
        return kws_test(&args[2..]);
    }
    if args.get(1).map(String::as_str) == Some("--mcp") {
        return mcp::serve(); // 给 Codex / Claude Code 提供桌面工具，不开界面
    }
    if args.get(1).map(String::as_str) == Some("--update") {
        return update::update_from_command_line(); // 命令行更新：检查、下载、替换，正在运行的 Aeri 会被重启
    }
    if args.get(1).map(String::as_str) == Some("--apply-update") {
        return update::apply(&args[2..]); // 新版本替换安装目录里的文件，然后启动新版
    }
    if !sys::single_instance() {
        return Ok(()); // 已经在运行了
    }
    // 软件渲染：不创建 GPU 上下文，窗口打开时也只多占几 MB
    slint::BackendSelector::new().backend_name("winit".into()).renderer_name("software".into()).select()?;

    let cfg = config::load();
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

    let icon = icon_rgba();
    let window = AppWindow::new()?;
    window.set_messages(ModelRc::from(Rc::new(VecModel::<ChatItem>::default())));
    let devices = Rc::new(RefCell::new(audio::input_device_names()));
    apply_config(&window, &cfg, &devices.borrow());
    window.set_listening_enabled(cfg.listen_on_start);
    history::set_enabled(cfg.save_history);
    history::prune();
    restore_history(&window);

    // 后台事件 -> 界面（必须回到界面线程更新）
    let weak = window.as_weak();
    let ui: assistant::UiSink = Arc::new(move |ev| {
        remote::observe(&ev); // 抄送给连着的手机
        history::observe(&ev);
        let weak = weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(w) = weak.upgrade() {
                on_ui_event(&w, ev);
            }
        });
    });
    runtime.spawn(assistant::run(cfg.clone(), cmd_rx, cmd_tx.clone(), ui));
    init_remote(&window, &cmd_tx, runtime.handle().clone());
    remote::apply(cfg.remote_enabled, cfg.remote_port);
    // 调试用：VA_AUTOTEST="命令1|命令2" 启动后自动执行这些文字命令
    if let Ok(cmds) = std::env::var("VA_AUTOTEST") {
        for c in cmds.split('|').filter(|c| !c.trim().is_empty()) {
            let _ = cmd_tx.send(Command::Text(c.trim().to_string()));
        }
    }

    wire_chat(&window, &cmd_tx);
    wire_settings(&window, &cmd_tx, devices, runtime.handle().clone());
    wire_update(&window, runtime.handle().clone(), cfg.auto_update);
    wire_history(&window);
    wire_plugins(&window, runtime.handle().clone());
    mark_settings_clean(&window);
    let _tray = create_tray(&window, cmd_tx.clone(), icon)?;

    // 悬浮球：单击展开主界面，关掉主界面再缩回悬浮球
    let (weak, tx) = (window.as_weak(), cmd_tx.clone());
    let created = ball::create(cfg.ball_position(), cfg.float_ball, move |event| {
        let (weak, tx) = (weak.clone(), tx.clone());
        // 窗口过程里不直接操作界面，排队回到 Slint 的事件循环里处理
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(w) = weak.upgrade() {
                on_ball_event(&w, &tx, event);
            }
        });
    });
    if let Err(e) = created {
        eprintln!("悬浮球创建失败：{e}");
    }
    sync_listening_menus(cfg.listen_on_start);
    apply_hotkey(&window, cfg.hotkey);
    let weak = window.as_weak();
    window.window().on_close_requested(move || {
        // 下次打开回到对话页；设置改了没保存时留在原处，不丢改动
        if let Some(w) = weak.upgrade() {
            if w.get_page() == 1 && !w.get_settings_dirty() {
                w.set_page(0);
                w.set_settings_note("".into());
            }
        }
        ball::show_if_enabled();
        slint::CloseRequestResponse::HideWindow
    });
    sys::refresh_autostart_path();

    let weak = window.as_weak();
    window.on_theme_changed(move || {
        if let Some(w) = weak.upgrade() {
            sync_title_bar(&w);
        }
    });

    // 设置页打开时检查有没有未保存的更改
    let settings_timer = slint::Timer::default();
    let weak = window.as_weak();
    settings_timer.start(slint::TimerMode::Repeated, Duration::from_millis(250), move || {
        if let Some(w) = weak.upgrade() {
            if w.get_page() == 1 && w.window().is_visible() {
                update_settings_dirty(&w);
            }
        }
    });

    // 窗口可见时每 2 秒刷新一次内存占用，顺便让标题栏跟上系统深浅色的变化
    let memory_timer = slint::Timer::default();
    let weak = window.as_weak();
    memory_timer.start(slint::TimerMode::Repeated, Duration::from_secs(2), move || {
        if let Some(w) = weak.upgrade() {
            if w.window().is_visible() {
                w.set_memory(format!("内存 {:.0} MB", sys::private_memory_mb()).into());
                sync_title_bar(&w);
                hook_native_window(&w);
            }
        }
    });

    // 开机自启时只在后台运行（悬浮球 / 托盘），不弹主界面
    let launched_at_login = args.iter().any(|a| a == "--autostart");
    if cfg.show_window_on_start && !launched_at_login {
        show_window(&window);
    } else {
        ball::show_if_enabled();
    }
    slint::run_event_loop_until_quit()?;
    drop(window);
    runtime.shutdown_timeout(Duration::from_millis(500));
    Ok(())
}

/// 用录音文件测试唤醒词：aeri.exe --kws-test a.wav b.wav ...
fn kws_test(files: &[String]) -> anyhow::Result<()> {
    let cfg = config::load();
    let (mut detector, problems) =
        kws::WakeDetector::new(&config::models_dir().join("kws"), &cfg.wake_words, cfg.kws_threshold, cfg.kws_score)?;
    for p in problems {
        println!("唤醒词问题：{p}");
    }
    for f in files {
        let wave = sherpa_onnx::Wave::read(f).ok_or_else(|| anyhow::anyhow!("读取失败：{f}"))?;
        let mut resampler = resample::Resampler::new(wave.sample_rate() as u32);
        // 前后各 1 秒带轻微底噪的安静段，按 10ms 一块喂入，和实际运行走同样的能量门限
        let noise = |n: usize| (0..n).map(|i| ((i * 7919 % 1000) as f32 / 1000.0 - 0.5) * 0.002).collect::<Vec<f32>>();
        let mut samples = noise(16000);
        samples.extend(resampler.process(wave.samples()));
        samples.extend(noise(16000));
        detector.reset();
        let mut gate = audio::EnergyGate::new();
        let mut hits = Vec::new();
        for chunk in samples.chunks(160) {
            for piece in gate.process(chunk, &mut detector) {
                hits.extend(detector.accept(&piece));
            }
        }
        println!("{f}: {}", if hits.is_empty() { "未检测到".into() } else { hits.join("、") });
    }
    Ok(())
}

fn on_ui_event(w: &AppWindow, ev: UiEvent) {
    #[cfg(debug_assertions)]
    match &ev {
        UiEvent::Status(s) => println!("[状态] {s:?}"),
        UiEvent::Chat(role, text) => println!("[{role:?}] {text}"),
        UiEvent::TaskProgress(id, text) => println!("[任务 #{id}] {text}"),
        UiEvent::TaskRunning(on) => println!("[后台任务进行中] {on}"),
        UiEvent::Mic(name) => println!("[麦克风] {name}"),
        UiEvent::Partial(_) => {}
        UiEvent::TextStarted(_) => {}
    }
    match ev {
        UiEvent::Status(s) => {
            w.set_status_code(s as i32);
            ball::set_state(s as i32);
        }
        UiEvent::Partial(p) => w.set_partial(p.into()),
        UiEvent::Mic(name) => w.set_mic_name(name.into()),
        UiEvent::Chat(role, text) => push_chat(w, role as i32, text, None),
        UiEvent::TaskProgress(id, text) => push_chat(w, assistant::Role::Tool as i32, format!("任务 #{id} · {text}"), Some(id)),
        UiEvent::TaskRunning(on) => {
            w.set_task_running(on);
            ball::set_task_running(on);
        }
        UiEvent::TextStarted(generation) => {
            if generation == assistant::text_generation() {
                w.set_pending_texts((w.get_pending_texts() - 1).max(0));
            }
        }
    }
}

/// 追加一条对话；replace_task 为某个任务的进展时，紧挨着的上一条同任务进展会被原地替换
fn push_chat(w: &AppWindow, role: i32, text: String, replace_task: Option<u32>) {
    let model = w.get_messages();
    let Some(vm) = model.as_any().downcast_ref::<VecModel<ChatItem>>() else { return };
    let item = ChatItem { role, text: text.into() };
    let last = vm.row_count().checked_sub(1);
    let same_task = |prev: &ChatItem| {
        replace_task.is_some_and(|id| prev.role == role && prev.text.starts_with(&format!("任务 #{id} · ")))
    };
    match last {
        Some(i) if vm.row_data(i).is_some_and(|prev| same_task(&prev)) => vm.set_row_data(i, item),
        _ => {
            vm.push(item);
            if vm.row_count() > MAX_CHAT_ITEMS {
                vm.remove(0);
            }
        }
    }
    // 等布局更新后再滚到底
    let weak = w.as_weak();
    slint::Timer::single_shot(Duration::from_millis(30), move || {
        if let Some(w) = weak.upgrade() {
            w.invoke_scroll_to_bottom();
        }
    });
}

fn wire_chat(window: &AppWindow, cmd_tx: &UnboundedSender<Command>) {
    let tx = cmd_tx.clone();
    let weak = window.as_weak();
    window.on_send_text(move |t| {
        let text = t.trim().to_string();
        if text.is_empty() { return; }
        let Some(w) = weak.upgrade() else { return };
        let generation = assistant::text_generation();
        // 入队时就显示，不能等当前语音会话返回才确认发送。
        if tx.send(Command::TextSubmitted { text: text.clone(), generation }).is_ok() {
            remote::note_chat(assistant::Role::User, &text);
            history::record_chat(assistant::Role::User, &text);
            push_chat(&w, assistant::Role::User as i32, text, None);
            w.set_pending_texts(w.get_pending_texts() + 1);
        } else {
            push_chat(&w, assistant::Role::System as i32, "发送失败，请重新启动助手后重试。".into(), None);
        }
    });
    let weak = window.as_weak();
    window.on_cancel_pending_texts(move || {
        assistant::cancel_pending_texts();
        if let Some(w) = weak.upgrade() {
            w.set_pending_texts(0);
            push_chat(&w, assistant::Role::System as i32, "已取消尚未开始处理的文字；正在处理的内容可点光球停止。".into(), None);
        }
    });
    let (tx, weak) = (cmd_tx.clone(), window.as_weak());
    window.on_talk(move || {
        if let Some(w) = weak.upgrade() {
            talk_or_stop(&w, &tx);
        }
    });
    let tx = cmd_tx.clone();
    window.on_toggle_listening(move |on| {
        sync_listening_menus(on);
        let _ = tx.send(Command::SetListening(on));
    });
    let tx = cmd_tx.clone();
    let weak = window.as_weak();
    window.on_clear_chat(move || {
        if let Some(w) = weak.upgrade() {
            if let Some(vm) = w.get_messages().as_any().downcast_ref::<VecModel<ChatItem>>() {
                vm.set_vec(Vec::new());
            }
        }
        remote::clear_history();
        let _ = tx.send(Command::ClearHistory);
    });
}

// ---------- 设置页 ----------

fn sensitivity_to_threshold(s: f32) -> f32 {
    (0.45 - 0.35 * s.clamp(0.0, 1.0)).clamp(0.05, 0.6)
}

fn threshold_to_sensitivity(t: f32) -> f32 {
    ((0.45 - t) / 0.35).clamp(0.0, 1.0)
}

fn set_devices(w: &AppWindow, devices: &[String], selected: &str) {
    let mut items: Vec<slint::SharedString> = vec!["系统默认".into()];
    items.extend(devices.iter().map(|d| d.as_str().into()));
    w.set_devices(ModelRc::from(Rc::new(VecModel::from(items))));
    let idx = devices.iter().position(|d| !selected.is_empty() && d.contains(selected)).map_or(0, |i| i + 1);
    w.set_device_index(idx as i32);
}

/// key 状态徽标：0 未设置  1 已就绪  2 测试中  3 测试通过  4 测试失败
const KEY_MISSING: i32 = 0;
const KEY_READY: i32 = 1;
const KEY_TESTING: i32 = 2;
const KEY_OK: i32 = 3;
const KEY_FAILED: i32 = 4;
const THEMES: [&str; 3] = ["auto", "light", "dark"];

fn refresh_key_status(w: &AppWindow, c: &Config) {
    VOICE_SAVED_KEY.set(config::asr_key(c));
    w.set_llm_key_status(config::key_status(&c.llm_api_key_protected, &c.llm_key_env).into());
    w.set_asr_key_status(config::key_status(&c.asr_api_key_protected, &c.asr_key_env).into());
    w.set_llm_key_state(if config::llm_key(c).is_some() { KEY_READY } else { KEY_MISSING });
    w.set_asr_key_state(if config::asr_key(c).is_some() { KEY_READY } else { KEY_MISSING });
    w.set_llm_key_saved(config::has_saved_key(&c.llm_api_key_protected));
    w.set_asr_key_saved(config::has_saved_key(&c.asr_api_key_protected));
}

/// 配置 -> 界面（key 本身从不显示，只显示状态）
fn apply_config(w: &AppWindow, c: &Config, devices: &[String]) {
    w.set_llm_key_input("".into());
    w.set_llm_base_url(c.llm_base_url.as_str().into());
    w.set_llm_model(c.llm_model.as_str().into());
    w.set_llm_thinking(c.llm_thinking);
    w.set_effort_index(EFFORTS.iter().position(|e| *e == c.llm_effort).unwrap_or(0) as i32);
    w.set_new_conversation_secs(c.new_conversation_after_secs as i32);
    w.set_asr_key_input("".into());
    w.set_asr_url(c.asr_url.as_str().into());
    w.set_asr_model(c.asr_model.as_str().into());
    w.set_asr_silence_ms(c.asr_sentence_silence_ms as i32);
    w.set_asr_timeout_secs(c.asr_speech_timeout_secs as i32);
    w.set_wake_words(c.wake_words.join("，").into());
    if let Some(first) = c.wake_words.first() {
        w.set_first_wake_word(first.as_str().into());
    }
    w.set_sensitivity(threshold_to_sensitivity(c.kws_threshold));
    w.set_kws_score(c.kws_score);
    w.set_follow_up(c.follow_up);
    w.set_duck_others(c.duck_others);
    w.set_auto_update(c.auto_update);
    w.set_save_history(c.save_history);
    w.set_listen_on_start(c.listen_on_start);
    set_devices(w, devices, &c.input_device);
    w.set_desktop_control(c.desktop_control);
    w.set_browser_control(c.browser_control);
    w.set_browser_idle_minutes(c.browser_idle_minutes as i32);
    w.set_playwright_package(c.playwright_mcp.as_str().into());
    w.set_tts_rate(c.tts_rate as f32);
    w.set_show_on_start(c.show_window_on_start);
    w.set_float_ball(c.float_ball);
    w.set_hotkey(c.hotkey);
    w.set_launch_at_login(sys::autostart_enabled()); // 以注册表为准
    w.set_executor_index(EXECUTORS.iter().position(|e| *e == c.executor).unwrap_or(0) as i32);
    w.set_access_index(ACCESS.iter().position(|e| *e == c.agent_access).unwrap_or(0) as i32);
    w.set_agent_workdir(c.agent_workdir.as_str().into());
    w.set_agent_model(c.agent_model.as_str().into());
    w.set_executor_status(executor_status().into());
    w.set_frontend_index(FRONTENDS.iter().position(|e| *e == c.voice_frontend).unwrap_or(0) as i32);
    w.set_realtime_model(c.realtime_model.as_str().into());
    w.set_realtime_url(c.realtime_url.as_str().into());
    w.set_realtime_voice(c.realtime_voice.as_str().into());
    w.set_barge_in(c.barge_in);
    w.set_full_duplex(c.full_duplex);
    w.set_remote_enabled(c.remote_enabled);
    w.global::<Theme>().set_scheme(THEMES.iter().position(|t| *t == c.theme).unwrap_or(0) as i32);
    refresh_key_status(w, c);
    invalidate_voice_test(w);
}

/// 界面 -> 配置（以已保存的配置为底，保留 key 和界面上没有的项）
fn read_settings(w: &AppWindow, devices: &[String]) -> Config {
    let mut c = config::load();
    fill_settings(w, devices, &mut c);
    c
}

fn fill_settings(w: &AppWindow, devices: &[String], c: &mut Config) {
    let text = |s: slint::SharedString| s.trim().to_string();
    c.llm_base_url = text(w.get_llm_base_url());
    c.llm_model = text(w.get_llm_model());
    c.llm_thinking = w.get_llm_thinking();
    c.llm_effort = EFFORTS[w.get_effort_index().clamp(0, 2) as usize].into();
    c.new_conversation_after_secs = w.get_new_conversation_secs().max(30) as u64;
    c.asr_url = text(w.get_asr_url());
    c.asr_model = text(w.get_asr_model());
    c.asr_sentence_silence_ms = w.get_asr_silence_ms().clamp(200, 6000) as u32;
    c.asr_speech_timeout_secs = w.get_asr_timeout_secs().clamp(2, 30) as u64;
    c.wake_words = w
        .get_wake_words()
        .split([',', '，', '、', ';', '；', ' '])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    c.kws_threshold = sensitivity_to_threshold(w.get_sensitivity());
    c.kws_score = (w.get_kws_score() * 10.0).round() / 10.0;
    c.follow_up = w.get_follow_up();
    c.duck_others = w.get_duck_others();
    c.auto_update = w.get_auto_update();
    c.save_history = w.get_save_history();
    c.listen_on_start = w.get_listen_on_start();
    let idx = w.get_device_index();
    c.input_device = if idx <= 0 { String::new() } else { devices.get(idx as usize - 1).cloned().unwrap_or_default() };
    c.desktop_control = w.get_desktop_control();
    c.browser_control = w.get_browser_control();
    c.browser_idle_minutes = w.get_browser_idle_minutes().clamp(1, 120) as u64;
    c.playwright_mcp = text(w.get_playwright_package());
    c.tts_rate = w.get_tts_rate().round() as i32;
    c.show_window_on_start = w.get_show_on_start();
    c.float_ball = w.get_float_ball();
    c.hotkey = w.get_hotkey();
    c.executor = EXECUTORS[w.get_executor_index().clamp(0, 2) as usize].into();
    c.agent_access = ACCESS[w.get_access_index().clamp(0, 2) as usize].into();
    c.agent_workdir = text(w.get_agent_workdir());
    c.agent_model = text(w.get_agent_model());
    c.voice_frontend = FRONTENDS[w.get_frontend_index().clamp(0, 1) as usize].into();
    c.realtime_model = text(w.get_realtime_model());
    c.realtime_url = text(w.get_realtime_url());
    c.realtime_voice = text(w.get_realtime_voice());
    c.barge_in = w.get_barge_in();
    c.full_duplex = w.get_full_duplex();
    c.remote_enabled = w.get_remote_enabled();
    c.theme = THEMES[w.global::<Theme>().get_scheme().clamp(0, 2) as usize].into();
}

thread_local! {
    /// 上次载入或保存时设置页的样子，用来判断有没有未保存的更改
    static SETTINGS_BASELINE: RefCell<Option<serde_json::Value>> = const { RefCell::new(None) };
    static VOICE_TEST_INPUT: RefCell<Option<serde_json::Value>> = const { RefCell::new(None) };
    static VOICE_SAVED_KEY: RefCell<Option<String>> = const { RefCell::new(None) };
}

// 只在内存比较，绝不记录凭证。更改当前链路后不沿用旧测试结果。
fn voice_test_snapshot(w: &AppWindow) -> serde_json::Value {
    let input = w.get_asr_key_input();
    let key = effective_voice_key(input.as_str(), VOICE_SAVED_KEY.with_borrow(Clone::clone));
    serde_json::json!({
        "mode": w.get_frontend_index(),
        "url": w.get_realtime_url().as_str(), "model": w.get_realtime_model().as_str(),
        "voice": w.get_realtime_voice().as_str(), "rate": w.get_tts_rate(),
        "asr_url": w.get_asr_url().as_str(), "asr_model": w.get_asr_model().as_str(),
        "silence": w.get_asr_silence_ms(), "wait": w.get_asr_timeout_secs(),
        "key": key,
    })
}

fn effective_voice_key(input: &str, saved: Option<String>) -> Option<String> {
    let input = input.trim();
    if input.is_empty() { saved } else { Some(input.to_string()) }
}

#[cfg(test)]
mod voice_settings_tests {
    use super::*;

    #[test]
    fn saving_tested_key_preserves_identity_but_editing_invalidates_it() {
        let tested = effective_voice_key("  test-key  ", None);
        let saved = effective_voice_key("", Some("test-key".into()));
        assert_eq!(tested, saved, "保存并清空输入后应保留测试结果");
        assert_ne!(tested, effective_voice_key("different-key", saved));
    }

    #[test]
    fn unused_realtime_fields_do_not_block_classic_mode_save() {
        let mut c = Config::default();
        c.voice_frontend = "asr".into();
        c.realtime_url.clear();
        c.realtime_model.clear();
        assert_eq!(validate(&c), None);
        c.voice_frontend = "realtime".into();
        assert!(validate(&c).is_some());
    }
}

fn invalidate_voice_test(w: &AppWindow) {
    w.set_voice_test_epoch(w.get_voice_test_epoch().wrapping_add(1));
    w.set_voice_test_state(KEY_READY);
    w.set_voice_test_status("尚未测试当前语音模式；连接测试不包含实际录音和出声。".into());
    VOICE_TEST_INPUT.set(None);
}

/// 设置页上所有可编辑内容的快照（不含悬浮球位置等界面外的项）
fn settings_snapshot(w: &AppWindow) -> serde_json::Value {
    let mut c = Config::default();
    fill_settings(w, &[], &mut c);
    serde_json::json!({
        "config": serde_json::to_value(&c).unwrap_or_default(),
        "device": w.get_device_index(),
        "autostart": w.get_launch_at_login(),
        "keys": [w.get_llm_key_input().trim(), w.get_asr_key_input().trim()],
    })
}

/// 当前界面上的设置就是已保存的状态
fn mark_settings_clean(w: &AppWindow) {
    SETTINGS_BASELINE.set(Some(settings_snapshot(w)));
    w.set_settings_dirty(false);
}

/// 设置页打开时定时比较，有改动就提示“未保存”
fn update_settings_dirty(w: &AppWindow) {
    let stale = VOICE_TEST_INPUT.with_borrow(|b| b.as_ref().is_some_and(|v| *v != voice_test_snapshot(w)));
    if stale { invalidate_voice_test(w); }
    let dirty = SETTINGS_BASELINE.with_borrow(|b| b.as_ref().is_some_and(|b| *b != settings_snapshot(w)));
    if dirty != w.get_settings_dirty() {
        w.set_settings_dirty(dirty);
        if dirty {
            set_note(w, NOTE_INFO, "");
        }
    }
}

/// 设置页底部提示：成功 / 出错 / 普通说明
const NOTE_OK: i32 = 0;
const NOTE_ERROR: i32 = 1;
const NOTE_INFO: i32 = 2;

fn set_note(w: &AppWindow, kind: i32, text: impl Into<slint::SharedString>) {
    w.set_note_kind(kind);
    w.set_settings_note(text.into());
}

/// 设置页“谁来干活”下面的说明：命令行装没装
fn executor_status() -> String {
    let found = |names: &[&str]| tasks::find_program(names).is_some();
    let codex = if found(&["codex.exe", "codex.cmd"]) { "已安装" } else { "未安装（npm install -g @openai/codex）" };
    let claude = if found(&["claude.exe", "claude.cmd"]) { "已安装，第一次用前要在终端运行 claude 登录" } else { "未安装" };
    format!("复杂任务交给它在后台做，做完会主动告诉你。Codex：{codex}；Claude Code：{claude}。")
}

/// 检查必填项，返回问题说明
fn validate(c: &Config) -> Option<String> {
    if c.wake_words.is_empty() {
        return Some("请至少填一个唤醒词".into());
    }
    if !c.llm_base_url.starts_with("http") {
        return Some("大模型接口地址应以 http 开头".into());
    }
    if !c.asr_url.starts_with("ws") {
        return Some("语音识别服务地址应以 wss:// 开头".into());
    }
    if c.voice_frontend == "realtime" && !c.realtime_url.starts_with("ws") {
        return Some("实时语音服务地址应以 wss:// 开头".into());
    }
    if c.llm_model.is_empty() || c.asr_model.is_empty() || (c.voice_frontend == "realtime" && c.realtime_model.is_empty()) {
        return Some("模型名不能为空".into());
    }
    if !c.agent_workdir.is_empty() && !std::path::Path::new(&c.agent_workdir).is_dir() {
        return Some("后台智能体的工作目录不存在".into());
    }
    None
}

// ---------- 软件更新 ----------

const UPDATE_CHECK: i32 = 0;
const UPDATE_INSTALL: i32 = 1;
const UPDATE_BUSY: i32 = 2;

fn set_update(weak: &slint::Weak<AppWindow>, action: i32, status: String) {
    let weak = weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(w) = weak.upgrade() {
            w.set_update_action(action);
            w.set_update_status(status.into());
        }
    });
}

fn found_text(rel: &update::Release) -> String {
    let summary = update::notes_summary(&rel.notes);
    let size = if rel.size > 0 { format!("（{:.1} MB）", rel.size as f64 / 1048576.0) } else { String::new() };
    if summary.is_empty() { format!("有新版本 v{}{size}", rel.version) } else { format!("有新版本 v{}{size}：
{summary}", rel.version) }
}

/// 检查更新；manual 为 false 时是自动检查，没有新版本或出错都不打扰
async fn run_check(weak: slint::Weak<AppWindow>, manual: bool) {
    match update::check().await {
        Ok(Some(rel)) => {
            set_update(&weak, UPDATE_INSTALL, found_text(&rel));
            let text = format!("发现新版本 v{}，可以在设置页底部点“更新”，或者直接对我说“更新软件”。", rel.version);
            update::set_latest(Some(rel));
            let weak2 = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = weak2.upgrade() {
                    push_chat(&w, 3, text, None);
                }
            });
        }
        Ok(None) => set_update(&weak, UPDATE_CHECK, format!("已经是最新版本（v{}）", update::CURRENT)),
        Err(e) if manual => set_update(&weak, UPDATE_CHECK, format!("检查失败：{e:#}")),
        Err(_) => set_update(&weak, UPDATE_CHECK, String::new()),
    }
}

/// 启动时把最近的对话显示回来，后面加一条分隔
fn restore_history(window: &AppWindow) {
    let items = history::recent_chat(40);
    let Some((day, _)) = items.last().cloned() else { return };
    let model = window.get_messages();
    let Some(vm) = model.as_any().downcast_ref::<VecModel<ChatItem>>() else { return };
    for (_, e) in items {
        vm.push(ChatItem { role: e.role, text: e.text.into() });
    }
    vm.push(ChatItem { role: assistant::Role::System as i32, text: format!("以上是之前的对话（{}）", &day[5..]).into() });
    let weak = window.as_weak();
    slint::Timer::single_shot(Duration::from_millis(300), move || {
        if let Some(w) = weak.upgrade() {
            w.invoke_scroll_to_bottom();
        }
    });
}

/// 插件页的列表：本地的（内置 + 已装）和市场里的合在一起
fn plugin_rows() -> Vec<PluginRow> {
    let local = plugins::all();
    let market = plugins::cached_market();
    let mut rows: Vec<PluginRow> = local
        .iter()
        .map(|p| {
            let update = market.iter().find(|l| l.id == p.id).filter(|l| plugins::newer(&l.version, &p.version));
            let (state, extra) = match (update, p.builtin) {
                (Some(l), _) => (2, format!(" · 可更新到 v{}", l.version)),
                (None, true) => (3, String::new()),
                (None, false) => (1, String::new()),
            };
            PluginRow { id: p.id.clone().into(), name: p.name.clone().into(), summary: format!("v{}{extra} · {}", p.version, p.description).into(), state }
        })
        .collect();
    for l in market.iter().filter(|l| !local.iter().any(|p| p.id == l.id)) {
        rows.push(PluginRow { id: l.id.clone().into(), name: l.name.clone().into(), summary: format!("v{} · {}", l.version, l.description).into(), state: 0 });
    }
    rows
}

fn wire_plugins(window: &AppWindow, rt: Handle) {
    window.set_plugins(ModelRc::from(Rc::new(VecModel::from(plugin_rows()))));
    let show = |weak: &slint::Weak<AppWindow>, status: String| {
        let weak = weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(w) = weak.upgrade() {
                w.set_plugins(ModelRc::from(Rc::new(VecModel::from(plugin_rows()))));
                w.set_plugins_status(status.into());
                w.set_plugins_busy("".into());
            }
        });
    };
    let (weak, rt2) = (window.as_weak(), rt.clone());
    window.on_plugins_refresh(move || {
        let Some(w) = weak.upgrade() else { return };
        w.set_plugins_busy("*".into());
        w.set_plugins_status("正在读取插件市场…".into());
        let weak = weak.clone();
        rt2.spawn(async move {
            let status = match plugins::fetch_market().await {
                Ok(m) => format!("插件市场共 {} 个插件 · github.com/{}", m.len(), plugins::MARKET_REPO),
                Err(e) => format!("{e}，先显示本机已有的插件"),
            };
            show(&weak, status);
        });
    });
    let weak = window.as_weak();
    window.on_plugin_action(move |id| {
        let Some(w) = weak.upgrade() else { return };
        let id = id.to_string();
        let state = plugin_rows().into_iter().find(|r| r.id == id.as_str()).map(|r| r.state);
        match state {
            Some(1) => {
                let status = match plugins::remove(&id) {
                    Ok(n) => format!("已卸载“{n}”"),
                    Err(e) => e,
                };
                show(&weak, status);
            }
            Some(0) | Some(2) => {
                w.set_plugins_busy(id.clone().into());
                let weak = weak.clone();
                rt.spawn(async move {
                    let status = match plugins::install(&id).await {
                        Ok(p) => format!("已安装“{}” v{}", p.name, p.version),
                        Err(e) => e,
                    };
                    show(&weak, status);
                });
            }
            _ => {}
        }
    });
}

fn wire_history(window: &AppWindow) {
    let weak = window.as_weak();
    window.on_history_open(move || {
        let Some(w) = weak.upgrade() else { return };
        match history::open_viewer() {
            Ok(()) => w.set_history_status("已在浏览器中打开".into()),
            Err(e) => w.set_history_status(format!("打开失败：{e}").into()),
        }
    });
    let weak = window.as_weak();
    window.on_history_clear(move || {
        let Some(w) = weak.upgrade() else { return };
        match history::clear() {
            Ok(()) => w.set_history_status("已清空全部历史记录".into()),
            Err(e) => w.set_history_status(format!("清空失败：{e}").into()),
        }
    });
}

fn wire_update(window: &AppWindow, rt: Handle, auto: bool) {
    window.set_app_version(format!("v{}", update::CURRENT).into());
    if let Some(text) = update::take_result() {
        push_chat(window, 3, text, None);
    }
    let weak = window.as_weak();
    let rt2 = rt.clone();
    window.on_update_clicked(move || {
        let rt = &rt2;
        let Some(w) = weak.upgrade() else { return };
        let weak = weak.clone();
        match w.get_update_action() {
            UPDATE_CHECK => {
                w.set_update_action(UPDATE_BUSY);
                w.set_update_status("正在检查…".into());
                rt.spawn(run_check(weak, true));
            }
            UPDATE_INSTALL => {
                let Some(rel) = update::latest() else { return };
                w.set_update_action(UPDATE_BUSY);
                w.set_update_status("正在下载…".into());
                rt.spawn(async move {
                    let progress_weak = weak.clone();
                    let version = rel.version.clone();
                    let r = update::download(&rel, move |p| set_update(&progress_weak, UPDATE_BUSY, format!("正在下载 v{version}：{p}%"))).await;
                    match r.and_then(|dir| update::launch_apply(&dir)) {
                        Ok(()) => {
                            set_update(&weak, UPDATE_BUSY, "下载完成，正在重启以完成更新…".into());
                            tokio::time::sleep(Duration::from_millis(800)).await;
                            let _ = slint::invoke_from_event_loop(|| { let _ = slint::quit_event_loop(); });
                        }
                        Err(e) => set_update(&weak, UPDATE_INSTALL, format!("更新失败：{e:#}")),
                    }
                });
            }
            _ => {}
        }
    });
    if auto && update::due() {
        let weak = window.as_weak();
        rt.spawn(async move {
            // 启动后先等一会儿，不和开机时的其他事抢网络
            tokio::time::sleep(Duration::from_secs(30)).await;
            run_check(weak, false).await;
        });
    }
}

fn wire_settings(window: &AppWindow, cmd_tx: &UnboundedSender<Command>, devices: Rc<RefCell<Vec<String>>>, rt: Handle) {
    // 保存并应用
    let (tx, weak, devs) = (cmd_tx.clone(), window.as_weak(), devices.clone());
    window.on_save_settings(move || {
        let Some(w) = weak.upgrade() else { return };
        let mut c = read_settings(&w, &devs.borrow());
        if let Some(problem) = validate(&c) {
            set_note(&w, NOTE_ERROR, problem);
            return;
        }
        for (input, slot) in [
            (w.get_llm_key_input(), &mut c.llm_api_key_protected),
            (w.get_asr_key_input(), &mut c.asr_api_key_protected),
        ] {
            if !input.trim().is_empty() {
                match config::protect_key(&input) {
                    Some(protected) => *slot = protected,
                    None => {
                        set_note(&w, NOTE_ERROR, "key 加密保存失败");
                        return;
                    }
                }
            }
        }
        match config::save(&c) {
            Ok(()) => {
                w.set_llm_key_input("".into());
                w.set_asr_key_input("".into());
                refresh_key_status(&w, &c);
                ball::set_enabled(c.float_ball);
                apply_hotkey(&w, c.hotkey);
                let (kind, note) = match sys::set_autostart(w.get_launch_at_login()) {
                    Ok(()) => (NOTE_OK, "已保存并生效".to_string()),
                    Err(e) => (NOTE_ERROR, format!("已保存，但开机自启设置失败：{e}")),
                };
                w.set_launch_at_login(sys::autostart_enabled());
                mark_settings_clean(&w);
                set_note(&w, kind, note);
                remote::apply(c.remote_enabled, c.remote_port);
                history::set_enabled(c.save_history);
                let _ = tx.send(Command::Reload(c));
            }
            Err(e) => set_note(&w, NOTE_ERROR, format!("保存失败：{e}")),
        }
    });

    // 恢复默认（不动已保存的 key，点“保存并应用”后才生效）
    let (weak, devs) = (window.as_weak(), devices.clone());
    window.on_reset_settings(move || {
        let Some(w) = weak.upgrade() else { return };
        let saved = config::load();
        let defaults = Config {
            llm_api_key_protected: saved.llm_api_key_protected,
            asr_api_key_protected: saved.asr_api_key_protected,
            ..Config::default()
        };
        apply_config(&w, &defaults, &devs.borrow());
        update_settings_dirty(&w);
        set_note(&w, NOTE_INFO, "已恢复默认值（API key 保留），点“保存并应用”生效");
    });

    // 放弃未保存的更改：界面回到已保存的配置
    let (weak, devs) = (window.as_weak(), devices.clone());
    window.on_discard_settings(move || {
        let Some(w) = weak.upgrade() else { return };
        apply_config(&w, &config::load(), &devs.borrow());
        mark_settings_clean(&w);
        set_note(&w, NOTE_INFO, "");
    });

    // 刷新麦克风列表（插拔设备后）
    let (weak, devs) = (window.as_weak(), devices);
    window.on_refresh_devices(move || {
        let Some(w) = weak.upgrade() else { return };
        let idx = w.get_device_index();
        let current = if idx > 0 { devs.borrow().get(idx as usize - 1).cloned().unwrap_or_default() } else { String::new() };
        *devs.borrow_mut() = audio::input_device_names();
        set_devices(&w, &devs.borrow(), &current);
        set_note(&w, NOTE_INFO, format!("找到 {} 个麦克风", devs.borrow().len()));
    });

    // 清除已保存的 key
    for asr in [false, true] {
        let (tx, weak) = (cmd_tx.clone(), window.as_weak());
        let clear = move || {
            let Some(w) = weak.upgrade() else { return };
            let mut c = config::load();
            if asr { c.asr_api_key_protected.clear() } else { c.llm_api_key_protected.clear() }
            if config::save(&c).is_ok() {
                refresh_key_status(&w, &c);
                set_note(&w, NOTE_OK, "已清除保存的 key");
                let _ = tx.send(Command::Reload(c));
            }
        };
        if asr { window.on_clear_asr_key(clear) } else { window.on_clear_llm_key(clear) }
    }

    // 测试大模型：用界面上当前填的值（还没保存也可以测）
    let (weak, rt2) = (window.as_weak(), rt.clone());
    window.on_test_llm(move || {
        let Some(w) = weak.upgrade() else { return };
        let c = read_settings(&w, &[]);
        let input = w.get_llm_key_input().trim().to_string();
        let Some(key) = (if input.is_empty() { config::llm_key(&c) } else { Some(input) }) else {
            w.set_llm_key_state(KEY_MISSING);
            w.set_llm_key_status("请先在上面粘贴 key".into());
            return;
        };
        w.set_llm_key_state(KEY_TESTING);
        w.set_llm_key_status("正在连接…".into());
        let weak = weak.clone();
        rt2.spawn(async move {
            let result = match llm::Llm::new(&c.llm_base_url, key, &c.llm_model, false, &c.llm_effort) {
                Ok(l) => l.ping().await,
                Err(e) => Err(e),
            };
            let (state, msg) = match result {
                Ok(()) => (KEY_OK, format!("测试通过：{} 可以正常调用", c.llm_model)),
                Err(e) => (KEY_FAILED, format!("测试失败：{e:#}")),
            };
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = weak.upgrade() {
                    w.set_llm_key_state(state);
                    w.set_llm_key_status(msg.into());
                }
            });
        });
    });

    // 测试正在编辑的语音模式，不保存配置，也不采集/播放音频。
    let (weak, voice_rt) = (window.as_weak(), rt.clone());
    window.on_test_voice(move || {
        let Some(w) = weak.upgrade() else { return };
        if w.get_voice_test_state() == KEY_TESTING { return; }
        let c = read_settings(&w, &[]);
        let input = w.get_asr_key_input().trim().to_string();
        let Some(key) = (if input.is_empty() { config::asr_key(&c) } else { Some(input) }) else {
            w.set_voice_test_state(KEY_MISSING);
            w.set_voice_test_status("请先填写顶部的百炼 key，再测试当前模式。".into());
            return;
        };
        let snapshot = voice_test_snapshot(&w);
        VOICE_TEST_INPUT.set(Some(snapshot.clone()));
        let epoch = w.get_voice_test_epoch().wrapping_add(1);
        w.set_voice_test_epoch(epoch);
        w.set_voice_test_state(KEY_TESTING);
        w.set_voice_test_status("正在验证当前语音服务连接和会话参数…".into());
        let weak = weak.clone();
        voice_rt.spawn(async move {
            let realtime_mode = c.voice_frontend == "realtime";
            let label = if realtime_mode { "实时语音" } else { "语音识别" };
            let result = tokio::time::timeout(Duration::from_secs(20), async {
                if realtime_mode {
                    let settings = realtime::RealtimeSettings {
                        url: c.realtime_url.clone(), model: c.realtime_model.clone(),
                        voice: c.realtime_voice.clone(), api_key: key.clone(),
                        instructions: realtime::VOICE_PROMPT.to_string(), tools: Vec::new(),
                        speech_rate: (1.0 + c.tts_rate as f32 * 0.1).clamp(0.5, 2.0),
                    };
                    let session = realtime::Session::connect(&settings).await?;
                    // 不请求回复；配置确认后有界关闭测试会话。
                    let _ = tokio::time::timeout(Duration::from_secs(2), session.close()).await;
                    Ok::<(), anyhow::Error>(())
                } else {
                    asr::check(&asr::AsrSettings {
                        url: c.asr_url.clone(), model: c.asr_model.clone(), api_key: key.clone(),
                        sentence_silence_ms: c.asr_sentence_silence_ms,
                        speech_timeout: Duration::from_secs(c.asr_speech_timeout_secs),
                    }).await
                }
            }).await;
            let (state, msg) = match result {
                Ok(Ok(())) => (KEY_OK, format!("{label}连接测试通过；未测试麦克风拾音和声音播放。修改后请保存并应用。")),
                Ok(Err(_)) => (KEY_FAILED, format!("{label}连接失败，请检查凭证、模型/音色是否开通及服务地址，然后重试。")),
                Err(_) => (KEY_FAILED, format!("{label}连接测试超时，请检查网络与服务地址后重试。")),
            };
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = weak.upgrade() {
                    if w.get_voice_test_epoch() == epoch {
                        if voice_test_snapshot(&w) == snapshot {
                            w.set_voice_test_state(state);
                            w.set_voice_test_status(msg.into());
                        } else { invalidate_voice_test(&w); }
                    }
                }
            });
        });
    });

    // 仅测试语音识别，独立于当前实时模式。
    let weak = window.as_weak();
    window.on_test_asr(move || {
        let Some(w) = weak.upgrade() else { return };
        let c = read_settings(&w, &[]);
        let input = w.get_asr_key_input().trim().to_string();
        let Some(key) = (if input.is_empty() { config::asr_key(&c) } else { Some(input) }) else {
            w.set_asr_key_state(KEY_MISSING);
            w.set_asr_key_status("请先在上面粘贴 key".into());
            return;
        };
        w.set_asr_key_state(KEY_TESTING);
        w.set_asr_key_status("正在连接…".into());
        let weak = weak.clone();
        rt.spawn(async move {
            let settings = asr::AsrSettings {
                url: c.asr_url.clone(),
                model: c.asr_model.clone(),
                api_key: key,
                sentence_silence_ms: c.asr_sentence_silence_ms,
                speech_timeout: Duration::from_secs(c.asr_speech_timeout_secs),
            };
            let (state, msg) = match asr::check(&settings).await {
                Ok(()) => (KEY_OK, format!("测试通过：{} 可以正常连接", c.asr_model)),
                Err(e) => (KEY_FAILED, format!("测试失败：{e:#}")),
            };
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = weak.upgrade() {
                    w.set_asr_key_state(state);
                    w.set_asr_key_status(msg.into());
                }
            });
        });
    });
}

// ---------- 窗口 ----------

thread_local! {
    static TITLE_BAR_DARK: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// 让 Windows 原生标题栏和界面背景同色（Windows 11 支持；只在深浅色变化时调用）
fn sync_title_bar(w: &AppWindow) {
    use slint::winit_030::WinitWindowAccessor;
    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::winit_030::winit::window::Theme as WinitTheme;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DwmSetWindowAttribute};

    let theme = w.global::<Theme>();
    let dark = theme.get_dark();
    if TITLE_BAR_DARK.get() == Some(dark) {
        return;
    }
    // COLORREF 是 0x00BBGGRR
    let colorref = |c: slint::Color| c.red() as u32 | (c.green() as u32) << 8 | (c.blue() as u32) << 16;
    let (caption, text) = (colorref(theme.get_canvas()), colorref(theme.get_ink()));
    // 窗口还没真正创建时返回 None，下次定时器再试
    let applied = w.window().with_winit_window(|win| {
        // 最小化/关闭按钮的深浅跟随主题
        win.set_theme(Some(if dark { WinitTheme::Dark } else { WinitTheme::Light }));
        if let Ok(handle) = win.window_handle() {
            if let RawWindowHandle::Win32(h) = handle.as_raw() {
                let hwnd = HWND(h.hwnd.get() as *mut _);
                unsafe {
                    let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &caption as *const u32 as *const _, 4);
                    let _ = DwmSetWindowAttribute(hwnd, DWMWA_TEXT_COLOR, &text as *const u32 as *const _, 4);
                }
            }
        }
    });
    if applied.is_some() {
        TITLE_BAR_DARK.set(Some(dark));
    }
}

thread_local! {
    static HOOKED_WINDOW: RefCell<Option<(isize, slint::Weak<AppWindow>)>> = const { RefCell::new(None) };
}

/// 原生窗口创建后补两件 Slint 没管好的事（窗口还没真正创建时什么都不做，下次再试）：
/// - 标题栏和任务栏图标：用 exe 里的品牌图标；
/// - Windows 丢掉窗口内容要求重画时（锁屏、切换桌面、外部隐藏再显示等），Slint 只重画它认为变了的地方，
///   其余部分会变成透明。这里接住带重画区域的 WM_PAINT，让整个窗口重画一遍。
fn hook_native_window(w: &AppWindow) {
    use slint::winit_030::WinitWindowAccessor;
    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::SetWindowSubclass;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW, SM_CXICON, SM_CXSMICON, SendMessageW, WM_SETICON,
    };
    use windows::core::PCWSTR;

    let Some(hwnd) = w.window().with_winit_window(|win| match win.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => Some(h.hwnd.get()),
        _ => None,
    }).flatten() else { return };
    if HOOKED_WINDOW.with_borrow(|h| h.as_ref().is_some_and(|(old, _)| *old == hwnd)) {
        return;
    }
    HOOKED_WINDOW.set(Some((hwnd, w.as_weak())));
    let hwnd = HWND(hwnd as *mut _);
    unsafe {
        let _ = SetWindowSubclass(hwnd, Some(repaint_on_invalidate), 1, 0);
        // app.rc 里的 1 号图标资源，按系统要求的大小各取一份，比缩放 RGBA 清楚
        let module = GetModuleHandleW(None).unwrap_or_default();
        for (kind, metric) in [(ICON_BIG, SM_CXICON), (ICON_SMALL, SM_CXSMICON)] {
            let size = GetSystemMetrics(metric);
            if let Ok(icon) = LoadImageW(Some(module.into()), PCWSTR(1 as _), IMAGE_ICON, size, size, LR_DEFAULTCOLOR) {
                SendMessageW(hwnd, WM_SETICON, Some(WPARAM(kind as usize)), Some(LPARAM(icon.0 as isize)));
            }
        }
    }
}

unsafe extern "system" fn repaint_on_invalidate(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
    _id: usize,
    _data: usize,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Graphics::Gdi::GetUpdateRect;
    use windows::Win32::UI::Shell::DefSubclassProc;
    use windows::Win32::UI::WindowsAndMessaging::WM_PAINT;

    // Slint 自己要求的重画没有重画区域；有区域说明是系统把内容丢了
    if msg == WM_PAINT && unsafe { GetUpdateRect(hwnd, None, false) }.as_bool() {
        // 窗口过程里不直接动界面，排队回到事件循环
        let _ = slint::invoke_from_event_loop(|| {
            if let Some(w) = HOOKED_WINDOW.with_borrow(|h| h.as_ref().and_then(|(_, weak)| weak.upgrade())) {
                w.set_repaint_epoch(w.get_repaint_epoch().wrapping_add(1));
            }
        });
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// 显示主界面（悬浮球随之隐藏）
fn show_window(w: &AppWindow) {
    use slint::winit_030::WinitWindowAccessor;
    let _ = w.show();
    w.window().set_minimized(false);
    // 悬浮球点击不抢焦点，展开时要主动把主界面切到最前面，否则会压在当前窗口后面
    w.window().with_winit_window(|win| win.focus_window());
    ball::set_visible(false);
    sync_title_bar(w);
    hook_native_window(w);
    // 启动时原生窗口要等事件循环跑起来才创建
    let weak = w.as_weak();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(w) = weak.upgrade() {
            hook_native_window(&w);
        }
    });
}

/// 从悬浮球展开：主界面出现在悬浮球旁边（球在屏幕右半边就放左侧，反之放右侧），底边对齐
fn open_from_ball(w: &AppWindow) {
    if let Some(r) = ball::rect() {
        let scale = w.window().scale_factor();
        let size = w.window().size();
        let (ww, wh) = if size.width > 0 {
            (size.width as i32, size.height as i32 + (32.0 * scale) as i32) // 加上标题栏
        } else {
            ((460.0 * scale) as i32, (792.0 * scale) as i32)
        };
        let work = ball::work_area();
        let gap = (8.0 * scale) as i32;
        let on_right = (r.left + r.right) / 2 > (work.left + work.right) / 2;
        let x = if on_right { r.left - gap - ww } else { r.right + gap };
        let y = r.bottom - wh;
        let x = x.clamp(work.left, (work.right - ww).max(work.left));
        let y = y.clamp(work.top, (work.bottom - wh).max(work.top));
        w.window().set_position(slint::PhysicalPosition::new(x, y));
    }
    show_window(w);
}

/// 光球 / 悬浮球“开始说话”：空闲时开始听；正在听、思考、执行或回答时停下
fn talk_or_stop(w: &AppWindow, tx: &UnboundedSender<Command>) {
    if (1..=4).contains(&w.get_status_code()) {
        assistant::interrupt();
    } else {
        let _ = tx.send(Command::Talk);
    }
}

/// 注册或取消全局快捷键，结果显示在设置页和空状态提示里
fn apply_hotkey(w: &AppWindow, on: bool) {
    let ok = ball::set_hotkey(on);
    w.set_hotkey_label(if on && ok { ball::HOTKEY_LABEL.into() } else { "".into() });
    w.set_hotkey_note(
        if !ok {
            format!("{} 已被其他程序占用，没能启用", ball::HOTKEY_LABEL)
        } else {
            format!("按 {} 开始说话；听、思考或回答时再按一下停止", ball::HOTKEY_LABEL)
        }
        .into(),
    );
}

fn toggle_listening(w: &AppWindow, tx: &UnboundedSender<Command>) {
    set_listening(w, tx, !w.get_listening_enabled());
}

fn set_listening(w: &AppWindow, tx: &UnboundedSender<Command>, on: bool) {
    w.set_listening_enabled(on);
    sync_listening_menus(on);
    let _ = tx.send(Command::SetListening(on));
}

// ---------- 手机控制 ----------

fn init_remote(window: &AppWindow, cmd_tx: &UnboundedSender<Command>, rt: Handle) {
    let (weak, tx) = (window.as_weak(), cmd_tx.clone());
    let set_listening_hook = move |on: bool| {
        let (weak, tx) = (weak.clone(), tx.clone());
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(w) = weak.upgrade() {
                set_listening(&w, &tx, on);
            }
        });
    };
    let weak = window.as_weak();
    let changed_hook = move || {
        let weak = weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(w) = weak.upgrade() {
                refresh_remote(&w);
            }
        });
    };
    remote::init(
        rt,
        remote::Hooks { commands: cmd_tx.clone(), set_listening: Box::new(set_listening_hook), changed: Box::new(changed_hook) },
    );
    window.on_start_pairing(|| {
        remote::start_pairing();
    });
    window.on_cancel_pairing(remote::cancel_pairing);
    window.on_unpair_all(remote::unpair_all);
    refresh_remote(window);
}

fn refresh_remote(w: &AppWindow) {
    let info = remote::info();
    w.set_remote_running(info.running);
    w.set_remote_code(info.code.into());
    w.set_remote_paired(info.paired as i32);
    w.set_remote_status(info.status.into());
}

thread_local! {
    static TRAY_TOGGLE: RefCell<Option<tray_icon::menu::MenuItem>> = const { RefCell::new(None) };
}

/// 悬浮球和托盘菜单里的“暂停 / 恢复语音唤醒”跟着当前状态走
fn sync_listening_menus(on: bool) {
    ball::set_listening(on);
    TRAY_TOGGLE.with_borrow(|item| {
        if let Some(item) = item {
            item.set_text(if on { "暂停语音唤醒" } else { "恢复语音唤醒" });
        }
    });
}

fn set_float_ball(w: &AppWindow, on: bool) {
    let mut c = config::load();
    c.float_ball = on;
    let _ = config::save(&c);
    let dirty = w.get_settings_dirty();
    w.set_float_ball(on);
    if !dirty {
        mark_settings_clean(w);
    }
    ball::set_enabled(on);
    if on && !w.window().is_visible() {
        ball::set_visible(true);
    }
}

fn on_ball_event(w: &AppWindow, tx: &UnboundedSender<Command>, event: ball::BallEvent) {
    use ball::BallEvent;
    match event {
        BallEvent::Open => open_from_ball(w),
        BallEvent::Talk => talk_or_stop(w, tx),
        BallEvent::ToggleListening => toggle_listening(w, tx),
        BallEvent::Hide => set_float_ball(w, false),
        BallEvent::Quit => {
            let _ = slint::quit_event_loop();
        }
        BallEvent::Moved(x, y) => {
            let mut c = config::load();
            (c.ball_x, c.ball_y) = (x, y);
            let _ = config::save(&c);
        }
    }
}

// ---------- 托盘 ----------

fn create_tray(window: &AppWindow, cmd_tx: UnboundedSender<Command>, icon: Vec<u8>) -> anyhow::Result<tray_icon::TrayIcon> {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let menu = Menu::new();
    let open = MenuItem::new("打开主界面", true, None);
    let toggle = MenuItem::new("暂停语音唤醒", true, None);
    let show_ball = MenuItem::new("显示悬浮球", true, None);
    let quit = MenuItem::new("退出", true, None);
    menu.append_items(&[&open, &toggle, &show_ball, &PredefinedMenuItem::separator(), &quit])?;
    let ids = (open.id().clone(), toggle.id().clone(), show_ball.id().clone(), quit.id().clone());
    TRAY_TOGGLE.set(Some(toggle.clone()));

    let weak = window.as_weak();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let (weak, tx) = (weak.clone(), cmd_tx.clone());
        let (open_id, toggle_id, ball_id, quit_id) = ids.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(w) = weak.upgrade() else { return };
            if e.id == open_id {
                show_window(&w);
            } else if e.id == toggle_id {
                toggle_listening(&w, &tx);
            } else if e.id == ball_id {
                set_float_ball(&w, true);
            } else if e.id == quit_id {
                let _ = slint::quit_event_loop();
            }
        });
    }));

    let weak = window.as_weak();
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
            let weak = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = weak.upgrade() {
                    show_window(&w);
                }
            });
        }
    }));

    Ok(TrayIconBuilder::new()
        .with_tooltip("Aeri 语音助手")
        .with_icon(tray_icon::Icon::from_rgba(icon, 32, 32)?)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()?)
}

/// 托盘与 Windows 应用图标使用同一份品牌图形。
fn icon_rgba() -> Vec<u8> {
    include_bytes!("../assets/app-32.rgba").to_vec()
}
