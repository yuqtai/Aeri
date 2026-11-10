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
mod locale;
mod ocr;
mod notify;
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
/// Recognition providers in the order shown in Settings. "Custom" covers every other service:
/// the address decides the format (OpenAI transcription API over https, Alibaba streaming over wss).
const ASR_PROVIDERS: [&str; 8] = ["local", "openai", "groq", "deepgram", "elevenlabs", "azure", "gemini", "custom"];
/// Realtime providers in the order shown in Settings; "Custom" also covers Alibaba DashScope / Qwen.
const RT_PROVIDERS: [&str; 4] = ["openai", "azure", "xai", "custom"];

fn is_alibaba_asr(url: &str) -> bool {
    url.starts_with("ws") && url.contains("/api-ws/")
}

fn is_alibaba_realtime(url: &str) -> bool {
    url.contains("dashscope") || url.contains("/api-ws/")
}

fn asr_index(id: &str) -> i32 {
    ASR_PROVIDERS.iter().position(|p| *p == id).unwrap_or(ASR_PROVIDERS.len() - 1) as i32
}

fn rt_index(id: &str) -> i32 {
    RT_PROVIDERS.iter().position(|p| *p == id).unwrap_or(RT_PROVIDERS.len() - 1) as i32
}

fn asr_id(index: i32, url: &str) -> &'static str {
    let id = ASR_PROVIDERS[index.clamp(0, ASR_PROVIDERS.len() as i32 - 1) as usize];
    if id == "custom" && is_alibaba_asr(url) { "dashscope" } else { id }
}

fn rt_id(index: i32, url: &str) -> &'static str {
    let id = RT_PROVIDERS[index.clamp(0, RT_PROVIDERS.len() as i32 - 1) as usize];
    if id == "custom" && is_alibaba_realtime(url) { "dashscope" } else { id }
}
const TTS_PROVIDERS: [&str; 5] = ["windows", "openai", "elevenlabs", "azure", "custom"];
const FRONTENDS: [&str; 2] = ["realtime", "classic"];

// Keep format literals checked by Rust while selecting UI copy centrally.
macro_rules! ui_format {
    ($en:literal, $zh:literal $(, $arg:expr)* $(,)?) => {{
        let en = format!($en $(, $arg)*);
        let zh = format!($zh $(, $arg)*);
        locale::pick(&en, &zh).to_owned()
    }};
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--kws-test") {
        return kws_test(&args[2..]);
    }
    if args.get(1).map(String::as_str) == Some("--notify") {
        notify::run_hook(args.get(2).map(String::as_str).unwrap_or("claude"));
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("--ocr-test") {
        let mut pt = windows::Win32::Foundation::POINT::default();
        unsafe { let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt); }
        println!("{}", ocr::text_near(pt.x, pt.y).unwrap_or_else(|| "(nothing)".into()));
        return Ok(());
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

    let first_run = config::first_run();
    let cfg = config::load();
    locale::apply_preference(&cfg.language);
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

    let icon = icon_rgba();
    let window = AppWindow::new()?;
    slint::select_bundled_translation(locale::slint_bundle())?;
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

    // Debug only: VA_UI_PAGE="settings-sub:key text" opens that settings page with the voice key field filled
    #[cfg(debug_assertions)]
    if let Ok(v) = std::env::var("VA_UI_PAGE") {
        if let Some((sub, key)) = v.split_once(':') {
            window.set_page(1);
            window.set_settings_sub(sub.parse().unwrap_or(1));
            if !key.is_empty() { window.set_asr_key_input(key.into()); }
        }
    }
    wire_chat(&window, &cmd_tx);
    wire_settings(&window, &cmd_tx, devices, runtime.handle().clone());
    wire_update(&window, runtime.handle().clone(), cfg.auto_update);
    wire_history(&window);
    wire_language(&window);
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
        eprintln!("{}", ui_format!("Could not create floating orb: {e}", "悬浮球创建失败：{e}"));
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
                w.set_memory(ui_format!("Memory in use: {:.0} MB", "当前内存 {:.0} MB", sys::private_memory_mb()).into());
                sync_title_bar(&w);
                hook_native_window(&w);
            }
        }
    });

    // 开机自启时只在后台运行（悬浮球 / 托盘），不弹主界面
    let launched_at_login = args.iter().any(|a| a == "--autostart");
    // First run: always open the main window so the user can enter a key and try a command
    if (cfg.show_window_on_start || first_run) && !launched_at_login {
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
        println!("{}", ui_format!("Wake word issue: {p}", "唤醒词问题：{p}"));
    }
    for f in files {
        let wave = sherpa_onnx::Wave::read(f).ok_or_else(|| anyhow::anyhow!(ui_format!("Could not read: {f}", "读取失败：{f}")))?;
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
        println!("{f}: {}", if hits.is_empty() { locale::pick("Not detected", "未检测到").into() } else { hits.join("、") });
    }
    Ok(())
}

fn on_ui_event(w: &AppWindow, ev: UiEvent) {
    #[cfg(debug_assertions)]
    match &ev {
        UiEvent::Status(s) => println!("{}", ui_format!("[Status] {s:?}", "[状态] {s:?}")),
        UiEvent::Chat(role, text) => println!("[{role:?}] {text}"),
        UiEvent::TaskProgress(id, text) => println!("{}", ui_format!("[Task #{id}] {text}", "[任务 #{id}] {text}")),
        UiEvent::TaskRunning(on) => println!("{}", ui_format!("[Background task running] {on}", "[后台任务进行中] {on}")),
        UiEvent::Mic(name) => println!("{}", ui_format!("[Microphone] {name}", "[麦克风] {name}")),
        UiEvent::Partial(_) => {}
        UiEvent::TextStarted(_) => {}
    }
    match ev {
        UiEvent::Status(s) => {
            w.set_status_code(s as i32);
            ball::set_state(s as i32);
        }
        UiEvent::Partial(p) => w.set_partial(p.into()),
        UiEvent::Mic(name) => {
            let name = if name == "麦克风不可用" { locale::pick("Microphone unavailable", "麦克风不可用") } else { &name };
            w.set_mic_name(name.into());
        },
        UiEvent::Chat(role, text) => push_chat(w, role as i32, text, None),
        UiEvent::TaskProgress(id, text) => push_chat(w, assistant::Role::Tool as i32, ui_format!("Task #{id} · {text}", "任务 #{id} · {text}"), Some(id)),
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
        replace_task.is_some_and(|id| prev.role == role && prev.text.starts_with(&ui_format!("Task #{id} · ", "任务 #{id} · ")))
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
            push_chat(&w, assistant::Role::System as i32, locale::pick("Could not send. Restart Aeri and try again.", "发送失败，请重新启动助手后重试。").into(), None);
        }
    });
    let weak = window.as_weak();
    window.on_cancel_pending_texts(move || {
        assistant::cancel_pending_texts();
        if let Some(w) = weak.upgrade() {
            w.set_pending_texts(0);
            push_chat(&w, assistant::Role::System as i32, locale::pick("Cancelled queued messages. Tap the orb to stop the current request.", "已取消尚未开始处理的文字；正在处理的内容可点光球停止。").into(), None);
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
    let mut items: Vec<slint::SharedString> = vec![locale::pick("System default", "系统默认").into()];
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
    VOICE_SAVED_KEY.set(if c.voice_frontend == "realtime" { config::realtime_key(c) } else { config::asr_key(c) });
    w.set_llm_key_status(ui_key_status(config::llm_protected(c), &c.llm_key_env).into());
    let voice_ready = if c.voice_frontend == "realtime" { config::realtime_key(c).is_some() } else { config::asr_key(c).is_some() };
    w.set_asr_key_status(if voice_ready {
        locale::pick("Ready (local, saved key, shared OpenAI key or environment variable)", "已就绪（本地识别、已保存的 key、共用的 OpenAI key 或环境变量）").into()
    } else {
        locale::pick("No API key for the selected speech provider", "所选语音服务还没有 API Key").into()
    });
    w.set_llm_key_state(if config::llm_key(c).is_some() { KEY_READY } else { KEY_MISSING });
    w.set_asr_key_state(if voice_ready { KEY_READY } else { KEY_MISSING });
    w.set_llm_key_saved(config::has_saved_key(config::llm_protected(c)));
    w.set_asr_key_saved(config::has_saved_key(config::voice_protected(c)));
    w.set_tts_key_saved(c.stt_api_keys_protected.get(&config::tts_slot(c)).is_some_and(|v| config::has_saved_key(v)));
    // Saved keys are shown (hidden behind the eye button) so you can always check what is stored
    let shown = |p: &str| slint::SharedString::from(config::unprotect_key(p).unwrap_or_default());
    w.set_llm_key_input(shown(config::llm_protected(c)));
    w.set_asr_key_input(shown(config::voice_protected(c)));
    w.set_tts_key_input(shown(c.stt_api_keys_protected.get(&config::tts_slot(c)).map(String::as_str).unwrap_or("")));
}

fn ui_key_status(protected: &str, env_name: &str) -> String {
    let status = config::key_status(protected, env_name);
    match status.as_str() {
        "已保存在本机" => locale::pick("Saved on this computer", "已保存在本机").into(),
        "未设置" => locale::pick("Not set", "未设置").into(),
        _ if status.starts_with("正在使用系统环境变量 ") => {
            ui_format!("Using environment variable {env_name}", "正在使用系统环境变量 {env_name}")
        }
        _ => status,
    }
}

/// 配置 -> 界面（key 本身从不显示，只显示状态）
fn apply_config(w: &AppWindow, c: &Config, devices: &[String]) {
    w.set_llm_providers(ModelRc::from(Rc::new(VecModel::from(llm::PRESETS.iter().map(|p| slint::SharedString::from(p.name)).collect::<Vec<_>>()))));
    w.set_llm_provider_index(llm::PRESETS.iter().position(|p| p.id == c.llm_provider).unwrap_or(7) as i32);
    w.set_asr_provider_index(asr_index(&c.asr_provider));
    w.set_realtime_provider_index(rt_index(&c.realtime_provider));
    w.set_llm_key_input("".into());
    w.set_llm_base_url(c.llm_base_url.as_str().into());
    w.set_llm_model(c.llm_model.as_str().into());
    w.set_llm_thinking(c.llm_thinking);
    w.set_effort_index(EFFORTS.iter().position(|e| *e == c.llm_effort).unwrap_or(0) as i32);
    w.set_new_conversation_secs(c.new_conversation_after_secs as i32);
    w.set_asr_key_input("".into());
    w.set_local_model_index(asr::LOCAL_MODELS.iter().position(|id| *id == c.local_asr_model).unwrap_or(0) as i32);
    refresh_local_model_status(w);
    w.set_asr_url(c.asr_url.as_str().into());
    w.set_asr_model(c.asr_model.as_str().into());
    w.set_asr_silence_ms(c.asr_sentence_silence_ms as i32);
    w.set_asr_timeout_secs(c.asr_speech_timeout_secs as i32);
    w.set_wake_words(c.wake_words.join(locale::pick(", ", "，")).into());
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
    w.set_tts_provider_index(TTS_PROVIDERS.iter().position(|p| *p == c.tts_provider).unwrap_or(0) as i32);
    let (model, voice) = tts::defaults(&c.tts_provider,locale::is_zh());
    w.set_tts_model(if c.tts_model.is_empty() { model } else { &c.tts_model }.into());
    w.set_tts_voice(if c.tts_voice.is_empty() { voice } else { &c.tts_voice }.into());
    w.set_tts_url(c.tts_url.as_str().into());
    w.set_azure_region(c.azure_speech_region.as_str().into());
    w.set_tts_key_input("".into());
    w.set_tts_test_status("".into());
    w.set_tts_test_state(KEY_READY);
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
    w.set_talk_barge(c.talk_barge);
    w.set_interrupt_index(if c.talk_barge { 0 } else if c.barge_in { 1 } else { 2 });
    w.set_remote_enabled(c.remote_enabled);
    w.set_language_index(locale::PREFERENCES.iter().position(|l| *l == c.language).unwrap_or(0) as i32);
    w.global::<Theme>().set_scheme(THEMES.iter().position(|t| *t == c.theme).unwrap_or(0) as i32);
    refresh_key_status(w, c);
    invalidate_voice_test(w);
}

fn refresh_local_model_status(w: &AppWindow) {
    let size = asr::local_size(asr::LOCAL_MODELS[w.get_local_model_index().clamp(0,6) as usize]);
    w.set_local_model_status(if size == 0 { locale::pick("Not downloaded", "尚未下载").into() }
        else { ui_format!("Downloaded: {:.1} MB on disk", "已下载：占用 {:.1} MB", size as f64 / 1048576.0).into() });
}

/// 界面 -> 配置（以已保存的配置为底，保留 key 和界面上没有的项）
fn read_settings(w: &AppWindow, devices: &[String]) -> Config {
    let mut c = config::load();
    fill_settings(w, devices, &mut c);
    c
}

fn fill_settings(w: &AppWindow, devices: &[String], c: &mut Config) {
    let text = |s: slint::SharedString| s.trim().to_string();
    let provider = &llm::PRESETS[w.get_llm_provider_index().clamp(0, 7) as usize];
    if c.llm_provider != provider.id { c.llm_key_env = provider.env.into(); }
    c.llm_provider = provider.id.into();
    c.asr_provider = asr_id(w.get_asr_provider_index(), w.get_asr_url().trim()).into();
    c.realtime_provider = rt_id(w.get_realtime_provider_index(), w.get_realtime_url().trim()).into();
    c.llm_base_url = text(w.get_llm_base_url());
    c.llm_model = text(w.get_llm_model());
    c.llm_thinking = w.get_llm_thinking();
    c.llm_effort = EFFORTS[w.get_effort_index().clamp(0, 2) as usize].into();
    c.new_conversation_after_secs = w.get_new_conversation_secs().max(30) as u64;
    c.local_asr_model = asr::LOCAL_MODELS[w.get_local_model_index().clamp(0,6) as usize].into();
    c.asr_url = text(w.get_asr_url());
    c.asr_model = text(w.get_asr_model());
    c.asr_sentence_silence_ms = w.get_asr_silence_ms().clamp(200, 6000) as u32;
    c.asr_speech_timeout_secs = w.get_asr_timeout_secs().clamp(2, 30) as u64;
    c.wake_words = config::parse_wake_words(&w.get_wake_words());
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
    c.tts_provider = TTS_PROVIDERS[w.get_tts_provider_index().clamp(0,4) as usize].into();
    c.tts_url = text(w.get_tts_url());
    c.tts_model = text(w.get_tts_model());
    c.tts_voice = text(w.get_tts_voice());
    c.azure_speech_region = text(w.get_azure_region());
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
    let interrupt = w.get_interrupt_index();
    c.talk_barge = interrupt == 0;
    c.barge_in = interrupt <= 1;
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
        "mode": w.get_frontend_index(), "provider": w.get_realtime_provider_index(), "asr_provider": w.get_asr_provider_index(),
        "llm_provider": w.get_llm_provider_index(), "llm_key": w.get_llm_key_input().as_str(),
        "url": w.get_realtime_url().as_str(), "model": w.get_realtime_model().as_str(),
        "voice": w.get_realtime_voice().as_str(), "rate": w.get_tts_rate(),
        "asr_url": w.get_asr_url().as_str(), "asr_model": w.get_asr_model().as_str(),
        "local_model": w.get_local_model_index(), "region": w.get_azure_region().as_str(),
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
        assert_eq!(tested, saved, "Keep test results after saving and clearing input");
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
    w.set_voice_test_status(locale::pick("Current voice mode has not been tested. Connection tests do not record or play audio.", "尚未测试当前语音模式；连接测试不包含实际录音和出声。").into());
    VOICE_TEST_INPUT.set(None);
}

/// 设置页上所有可编辑内容的快照（不含悬浮球位置等界面外的项）
fn settings_snapshot(w: &AppWindow) -> serde_json::Value {
    // Start from fixed (legacy) defaults, not Config::default(): new-install defaults depend on the
    // interface language, and switching language must not look like an unsaved change.
    let mut c: Config = serde_json::from_str("{}").unwrap_or_default();
    fill_settings(w, &[], &mut c);
    serde_json::json!({
        "config": serde_json::to_value(&c).unwrap_or_default(),
        "device": w.get_device_index(),
        "autostart": w.get_launch_at_login(),
        "keys": [w.get_llm_key_input().trim(), w.get_asr_key_input().trim(), w.get_tts_key_input().trim()],
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
    let codex = if found(&["codex.exe", "codex.cmd"]) { locale::pick("Installed", "已安装") } else { locale::pick("Not installed (npm install -g @openai/codex)", "未安装（npm install -g @openai/codex）") };
    let claude = if found(&["claude.exe", "claude.cmd"]) { locale::pick("Installed. Run claude in a terminal to sign in before first use", "已安装，第一次用前要在终端运行 claude 登录") } else { locale::pick("Not installed", "未安装") };
    ui_format!("Longer tasks run in the background and report back when done. Codex: {codex}; Claude Code: {claude}.", "复杂任务交给它在后台做，做完会主动告诉你。Codex：{codex}；Claude Code：{claude}。")
}

/// 检查必填项，返回问题说明
fn validate(c: &Config) -> Option<String> {
    if c.wake_words.is_empty() {
        return Some(locale::pick("Enter at least one wake word", "请至少填一个唤醒词").into());
    }
    if !c.llm_base_url.starts_with("http") {
        return Some(locale::pick("Language model API URL must start with http", "大模型接口地址应以 http 开头").into());
    }
    if c.voice_frontend != "realtime" && c.asr_provider == "dashscope" && !c.asr_url.starts_with("ws") {
        return Some(locale::pick("Speech recognition URL must start with wss://", "语音识别服务地址应以 wss:// 开头").into());
    }
    if c.voice_frontend != "realtime" && ["openai", "groq", "custom", "gemini", "elevenlabs"].contains(&c.asr_provider.as_str()) && !c.asr_url.starts_with("http") {
        return Some(locale::pick("Transcription base URL must start with https:// (or http:// for a local server)","转写接口地址应以 https:// 开头（本地服务可使用 http://）").into());
    }
    if c.voice_frontend == "realtime" && !c.realtime_url.starts_with("ws") {
        return Some(locale::pick("Realtime voice URL must start with wss://", "实时语音服务地址应以 wss:// 开头").into());
    }
    if c.llm_model.is_empty() || (c.asr_provider != "local" && c.asr_model.is_empty()) || (c.voice_frontend == "realtime" && c.realtime_model.is_empty()) {
        return Some(locale::pick("Model name cannot be empty", "模型名不能为空").into());
    }
    if !c.agent_workdir.is_empty() && !std::path::Path::new(&c.agent_workdir).is_dir() {
        return Some(locale::pick("The background agent working directory does not exist", "后台智能体的工作目录不存在").into());
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
    if summary.is_empty() { ui_format!("Update available: v{}{size}", "有新版本 v{}{size}", rel.version) } else { ui_format!("Update available: v{}{size}:\n{summary}", "有新版本 v{}{size}：\n{summary}", rel.version) }
}

/// 检查更新；manual 为 false 时是自动检查，没有新版本或出错都不打扰
async fn run_check(weak: slint::Weak<AppWindow>, manual: bool) {
    match update::check().await {
        Ok(Some(rel)) => {
            set_update(&weak, UPDATE_INSTALL, found_text(&rel));
            let text = ui_format!("Aeri v{} is available. Open Software updates in Settings, or say “update Aeri”.", "发现新版本 v{}，可以在设置页底部点“更新”，或者直接对我说“更新软件”。", rel.version);
            update::set_latest(Some(rel));
            let weak2 = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = weak2.upgrade() {
                    push_chat(&w, 3, text, None);
                }
            });
        }
        Ok(None) => set_update(&weak, UPDATE_CHECK, ui_format!("You’re up to date (v{})", "已经是最新版本（v{}）", update::CURRENT)),
        Err(e) if manual => set_update(&weak, UPDATE_CHECK, ui_format!("Check failed: {e:#}", "检查失败：{e:#}")),
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
    vm.push(ChatItem { role: assistant::Role::System as i32, text: ui_format!("Previous conversation ({})", "以上是之前的对话（{}）", &day[5..]).into() });
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
                (Some(l), _) => (2, ui_format!(" · Update to v{}", " · 可更新到 v{}", l.version)),
                (None, true) => (3, String::new()),
                (None, false) => (1, String::new()),
            };
            PluginRow { id: p.id.clone().into(), name: plugin_name(&p.name).into(), summary: format!("v{}{extra} · {}", p.version, plugin_description(&p.description)).into(), state }
        })
        .collect();
    for l in market.iter().filter(|l| !local.iter().any(|p| p.id == l.id)) {
        rows.push(PluginRow { id: l.id.clone().into(), name: plugin_name(&l.name).into(), summary: format!("v{} · {}", l.version, plugin_description(&l.description)).into(), state: 0 });
    }
    rows
}

fn plugin_name(name: &str) -> &str {
    match name {
        "记事本" => locale::pick("Notepad", name),
        "思源笔记" => locale::pick("SiYuan", name),
        _ => name,
    }
}

fn plugin_description(description: &str) -> &str {
    match description {
        "在记事本里写字、读取内容、保存" => locale::pick("Write, read, and save text in Notepad", description),
        "新建笔记、追加到每日笔记、打开或搜索笔记，用 Obsidian 自带的链接协议，不用点界面" => locale::pick("Create, append, open, and search notes using Obsidian links", description),
        "记笔记、追加到今天的日记、新建文档、搜索笔记，直接走思源的本机接口，不用点界面" => locale::pick("Create and search notes, and append to today’s journal using the local SiYuan API", description),
        _ => description,
    }
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
        w.set_plugins_status(locale::pick("Loading plugin marketplace…", "正在读取插件市场…").into());
        let weak = weak.clone();
        rt2.spawn(async move {
            let status = match plugins::fetch_market().await {
                Ok(m) => ui_format!("{} plugins in the marketplace · github.com/{}", "插件市场共 {} 个插件 · github.com/{}", m.len(), plugins::MARKET_REPO),
                Err(e) => ui_format!("{e}. Showing locally installed plugins", "{e}，先显示本机已有的插件"),
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
                    Ok(n) => ui_format!("Uninstalled “{n}”", "已卸载“{n}”"),
                    Err(e) => e,
                };
                show(&weak, status);
            }
            Some(0) | Some(2) => {
                w.set_plugins_busy(id.clone().into());
                let weak = weak.clone();
                rt.spawn(async move {
                    let status = match plugins::install(&id).await {
                        Ok(p) => ui_format!("Installed “{}” v{}", "已安装“{}” v{}", p.name, p.version),
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
            Ok(()) => w.set_history_status(locale::pick("Opened in your browser", "已在浏览器中打开").into()),
            Err(e) => w.set_history_status(ui_format!("Could not open: {e}", "打开失败：{e}").into()),
        }
    });
    let weak = window.as_weak();
    window.on_history_clear(move || {
        let Some(w) = weak.upgrade() else { return };
        match history::clear() {
            Ok(()) => w.set_history_status(locale::pick("All history cleared", "已清空全部历史记录").into()),
            Err(e) => w.set_history_status(ui_format!("Could not clear: {e}", "清空失败：{e}").into()),
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
                w.set_update_status(locale::pick("Checking…", "正在检查…").into());
                rt.spawn(run_check(weak, true));
            }
            UPDATE_INSTALL => {
                let Some(rel) = update::latest() else { return };
                w.set_update_action(UPDATE_BUSY);
                w.set_update_status(locale::pick("Downloading…", "正在下载…").into());
                rt.spawn(async move {
                    let progress_weak = weak.clone();
                    let version = rel.version.clone();
                    let r = update::download(&rel, move |p| set_update(&progress_weak, UPDATE_BUSY, ui_format!("Downloading v{version}: {p}%", "正在下载 v{version}：{p}%"))).await;
                    match r.and_then(|dir| update::launch_apply(&dir)) {
                        Ok(()) => {
                            set_update(&weak, UPDATE_BUSY, locale::pick("Download complete. Restarting to finish the update…", "下载完成，正在重启以完成更新…").into());
                            tokio::time::sleep(Duration::from_millis(800)).await;
                            let _ = slint::invoke_from_event_loop(|| { let _ = slint::quit_event_loop(); });
                        }
                        Err(e) => set_update(&weak, UPDATE_INSTALL, ui_format!("Update failed: {e:#}", "更新失败：{e:#}")),
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

/// Language takes effect right away and is saved immediately (it is not part of “Save and apply”):
/// the UI switches its bundled translation, and texts set from Rust are refreshed.
/// The tray and floating-orb menus are built at startup, so they follow after a restart.
fn wire_language(window: &AppWindow) {
    let weak = window.as_weak();
    window.on_language_changed(move |index| {
        let Some(w) = weak.upgrade() else { return };
        let pref = locale::PREFERENCES[index.clamp(0, 2) as usize];
        let mut saved = config::load();
        saved.language = pref.into();
        if config::save(&saved).is_err() {
            set_note(&w, NOTE_ERROR, locale::pick("Could not save the language setting", "语言设置保存失败"));
            return;
        }
        locale::apply_preference(pref);
        let _ = slint::select_bundled_translation(locale::slint_bundle());
        refresh_key_status(&w, &saved);
        refresh_remote(&w);
        w.set_history_status("".into());
        w.set_plugins_status("".into());
        w.set_language_note(locale::pick("The tray menu switches language after Aeri restarts.", "托盘菜单会在 Aeri 重启后切换语言。").into());
    });
}

fn wire_settings(window: &AppWindow, cmd_tx: &UnboundedSender<Command>, devices: Rc<RefCell<Vec<String>>>, rt: Handle) {
    let weak = window.as_weak();
    window.on_llm_provider_changed(move |index| {
        let Some(w) = weak.upgrade() else { return };
        let p = &llm::PRESETS[index.clamp(0, 7) as usize];
        w.set_llm_base_url(p.url.into());
        w.set_llm_model(p.model.into());
        w.set_llm_thinking(p.id == "deepseek");
        w.set_llm_key_input("".into());
        refresh_key_status(&w, &read_settings(&w, &[]));
    });
    let weak = window.as_weak();
    window.on_realtime_provider_changed(move |index| {
        let Some(w) = weak.upgrade() else { return };
        let defaults = realtime::Provider::defaults(RT_PROVIDERS[index.clamp(0, RT_PROVIDERS.len() as i32 - 1) as usize]);
        w.set_realtime_url(defaults.0.into());
        w.set_realtime_model(defaults.1.into());
        w.set_realtime_voice(defaults.2.into());
        w.set_asr_key_input("".into());
        let c = read_settings(&w, &[]);
        refresh_key_status(&w, &c);
        invalidate_voice_test(&w);
    });
    let weak = window.as_weak();
    window.on_asr_provider_changed(move |index| {
        let Some(w) = weak.upgrade() else { return };
        let defaults = match index {
            1 => ("https://api.openai.com/v1", "gpt-transcribe"),
            2 => ("https://api.groq.com/openai/v1", "whisper-large-v3-turbo"),
            3 => ("wss://api.deepgram.com/v1/listen", "nova-3"),
            4 => ("https://api.elevenlabs.io/v1", "scribe_v2"),
            5 => ("", "azure-speech"),
            6 => ("https://generativelanguage.googleapis.com/v1beta", "gemini-3.8-flash"),
            7 => ("http://localhost:8000/v1", "Qwen/Qwen3-ASR-1.7B"),
            _ => ("", ""),
        };
        w.set_asr_url(defaults.0.into());
        w.set_asr_model(defaults.1.into());
        w.set_asr_key_input("".into());
        refresh_key_status(&w, &read_settings(&w, &[]));
        invalidate_voice_test(&w);
    });
    let weak = window.as_weak();
    window.on_voice_mode_changed(move |_| {
        let Some(w) = weak.upgrade() else { return };
        w.set_asr_key_input("".into());
        refresh_key_status(&w, &read_settings(&w, &[]));
        invalidate_voice_test(&w);
    });
    let weak = window.as_weak();
    window.on_local_model_changed(move |_| {
        if let Some(w) = weak.upgrade() { refresh_local_model_status(&w); invalidate_voice_test(&w); }
    });
    let (weak,delete_rt) = (window.as_weak(),rt.clone());
    window.on_delete_local_model(move || {
        let Some(w) = weak.upgrade() else { return };
        let id = asr::LOCAL_MODELS[w.get_local_model_index().clamp(0,6) as usize];
        let weak = weak.clone();
        delete_rt.spawn(async move {
            let result = asr::delete_local(id).await;
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = weak.upgrade() {
                    refresh_local_model_status(&w);
                    if result.is_err() { w.set_local_model_status(locale::pick("Could not delete the model. Stop recognition and retry.","无法删除模型，请停止语音识别后重试。").into()); }
                }
            });
        });
    });
    let weak = window.as_weak();
    window.on_tts_provider_changed(move |index| {
        let Some(w) = weak.upgrade() else { return };
        let (model, voice) = tts::defaults(TTS_PROVIDERS[index.clamp(0,4) as usize],locale::is_zh());
        if index == 4 && w.get_tts_url().trim().is_empty() {
            w.set_tts_url("wss://cn-beijing.maas.qianwenaiapi.com/api-ws/v1/inference".into());
        }
        w.set_tts_model(model.into());
        w.set_tts_voice(voice.into());
        w.set_tts_key_input("".into());
        w.set_tts_test_status("".into());
        refresh_key_status(&w,&read_settings(&w,&[]));
    });
    let preview = std::sync::Arc::new(tts::Tts::new());
    let (weak, preview_rt, test_tts) = (window.as_weak(),rt.clone(),preview.clone());
    window.on_test_tts(move || {
        let Some(w) = weak.upgrade() else { return };
        if w.get_tts_test_state() == KEY_TESTING { test_tts.stop(); return; }
        let mut c = read_settings(&w,&[]);
        let input = w.get_tts_key_input();
        if !input.trim().is_empty() {
            c.stt_api_keys_protected.insert(config::tts_slot(&c), config::protect_key(input.as_str()).unwrap_or_default());
        } else if c.llm_provider == c.tts_provider && !w.get_llm_key_input().trim().is_empty() {
            *config::llm_key_slot(&mut c) = config::protect_key(w.get_llm_key_input().as_str()).unwrap_or_default();
        } else {
            let recognition_provider = if c.voice_frontend == "realtime" { &c.realtime_provider } else { &c.asr_provider };
            if *recognition_provider == c.tts_provider && !w.get_asr_key_input().trim().is_empty() {
                c.stt_api_keys_protected.insert(c.tts_provider.clone(),config::protect_key(w.get_asr_key_input().as_str()).unwrap_or_default());
            }
        }
        w.set_tts_test_state(KEY_TESTING);
        w.set_tts_test_status(locale::pick("Playing a short sample…", "正在播放简短示例…").into());
        let (weak, tts) = (weak.clone(),test_tts.clone());
        preview_rt.spawn(async move {
            let mut fallback = String::new();
            tts.speak_configured(&c,locale::pick("Hello, I'm Aeri. This is your selected voice.", "你好，我是 Aeri，这是您选择的音色。"),|note| fallback = note.to_string()).await;
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = weak.upgrade() {
                    w.set_tts_test_state(if fallback.is_empty() { KEY_OK } else { KEY_FAILED });
                    w.set_tts_test_status(if fallback.is_empty() { locale::pick("Voice preview finished or stopped.", "音色试听已结束或停止。").into() } else { fallback.into() });
                }
            });
        });
    });
    let (weak, tx) = (window.as_weak(),cmd_tx.clone());
    window.on_clear_tts_key(move || {
        let Some(w) = weak.upgrade() else { return };
        let mut c = config::load();
        let slot = if w.get_tts_provider_index() == 4 { config::TTS_CUSTOM_SLOT } else { TTS_PROVIDERS[w.get_tts_provider_index().clamp(0,4) as usize] };
        c.stt_api_keys_protected.remove(slot);
        if config::save(&c).is_ok() { refresh_key_status(&w,&c); let _ = tx.send(Command::Reload(c)); }
    });
    // 保存并应用
    let (tx, weak, devs) = (cmd_tx.clone(), window.as_weak(), devices.clone());
    window.on_save_settings(move || {
        let Some(w) = weak.upgrade() else { return };
        let mut c = read_settings(&w, &devs.borrow());
        if let Some(problem) = validate(&c) {
            set_note(&w, NOTE_ERROR, problem);
            return;
        }
        for (input, llm) in [(w.get_llm_key_input(), true), (w.get_asr_key_input(), false)] {
            if !input.trim().is_empty() {
                match config::protect_key(&input) {
                    Some(protected) => {
                        if llm { *config::llm_key_slot(&mut c) = protected; } else { *config::voice_key_slot(&mut c) = protected; }
                    }
                    None => { set_note(&w, NOTE_ERROR, locale::pick("Could not save the key", "key 保存失败")); return; }
                }
            }
        }
        if !w.get_tts_key_input().trim().is_empty() {
            let Some(protected) = config::protect_key(w.get_tts_key_input().as_str()) else {
                set_note(&w,NOTE_ERROR,locale::pick("Could not save the key", "key 保存失败")); return;
            };
            c.stt_api_keys_protected.insert(config::tts_slot(&c),protected);
        }
        match config::save(&c) {
            Ok(()) => {
                w.set_llm_key_input("".into());
                w.set_asr_key_input("".into());
                w.set_tts_key_input("".into());
                refresh_key_status(&w, &c);
                ball::set_enabled(c.float_ball);
                apply_hotkey(&w, c.hotkey);
                let (kind, note) = match sys::set_autostart(w.get_launch_at_login()) {
                    Ok(()) => (NOTE_OK, locale::pick("Saved and applied", "已保存并生效").to_string()),
                    Err(e) => (NOTE_ERROR, ui_format!("Saved, but could not set startup preference: {e}", "已保存，但开机自启设置失败：{e}")),
                };
                w.set_launch_at_login(sys::autostart_enabled());
                mark_settings_clean(&w);
                set_note(&w, kind, note);
                remote::apply(c.remote_enabled, c.remote_port);
                history::set_enabled(c.save_history);
                let _ = tx.send(Command::Reload(c));
            }
            Err(e) => set_note(&w, NOTE_ERROR, ui_format!("Could not save: {e}", "保存失败：{e}")),
        }
    });

    // 恢复默认（不动已保存的 key，点“保存并应用”后才生效）
    let (weak, devs) = (window.as_weak(), devices.clone());
    window.on_reset_settings(move || {
        let Some(w) = weak.upgrade() else { return };
        let saved = config::load();
        let defaults = Config {
            llm_api_key_protected: saved.llm_api_key_protected,
            llm_key_provider: saved.llm_key_provider,
            llm_api_keys_protected: saved.llm_api_keys_protected,
            asr_api_key_protected: saved.asr_api_key_protected,
            realtime_openai_api_key_protected: saved.realtime_openai_api_key_protected,
            stt_api_keys_protected: saved.stt_api_keys_protected,
            ..Config::default()
        };
        apply_config(&w, &defaults, &devs.borrow());
        update_settings_dirty(&w);
        set_note(&w, NOTE_INFO, locale::pick("Defaults restored (API keys kept). Click Save & apply to use them", "已恢复默认值（API key 保留），点“保存并应用”生效"));
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
        set_note(&w, NOTE_INFO, ui_format!("Found {} microphones", "找到 {} 个麦克风", devs.borrow().len()));
    });

    // 清除已保存的 key
    for asr in [false, true] {
        let (tx, weak) = (cmd_tx.clone(), window.as_weak());
        let clear = move || {
            let Some(w) = weak.upgrade() else { return };
            let mut c = config::load();
            if asr {
                if w.get_frontend_index() == 0 {
                    match rt_id(w.get_realtime_provider_index(), w.get_realtime_url().trim()) {
                        "openai" => c.realtime_openai_api_key_protected.clear(),
                        "dashscope" => c.asr_api_key_protected.clear(),
                        id => { c.stt_api_keys_protected.remove(&format!("realtime-{id}")); }
                    }
                } else {
                    let id = asr_id(w.get_asr_provider_index(), w.get_asr_url().trim());
                    if id == "dashscope" { c.asr_api_key_protected.clear(); }
                    else { c.stt_api_keys_protected.remove(id); }
                }
            } else {
                let id = llm::PRESETS[w.get_llm_provider_index().clamp(0, 7) as usize].id;
                if c.llm_key_provider == id { c.llm_api_key_protected.clear(); }
                else { c.llm_api_keys_protected.remove(id); }
            }
            if config::save(&c).is_ok() {
                refresh_key_status(&w, &c);
                set_note(&w, NOTE_OK, locale::pick("Saved key cleared", "已清除保存的 key"));
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
            w.set_llm_key_status(locale::pick("Paste a key above first", "请先在上面粘贴 key").into());
            return;
        };
        w.set_llm_key_state(KEY_TESTING);
        w.set_llm_key_status(locale::pick("Connecting…", "正在连接…").into());
        let weak = weak.clone();
        rt2.spawn(async move {
            let result = match llm::Llm::new(&c.llm_provider, &c.llm_base_url, key, &c.llm_model, false, &c.llm_effort) {
                Ok(l) => l.ping().await,
                Err(e) => Err(e),
            };
            let (state, msg) = match result {
                Ok(()) => (KEY_OK, ui_format!("Test passed: {} is available", "测试通过：{} 可以正常调用", c.llm_model)),
                Err(e) => (KEY_FAILED, ui_format!("Test failed: {e:#}", "测试失败：{e:#}")),
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
        let mut c = read_settings(&w, &[]);
        if (c.llm_provider == "openai" || c.llm_provider == "gemini") && !w.get_llm_key_input().trim().is_empty() {
            *config::llm_key_slot(&mut c) = config::protect_key(w.get_llm_key_input().as_str()).unwrap_or_default();
        }
        let input = w.get_asr_key_input().trim().to_string();
        let Some(key) = (if input.is_empty() { if c.voice_frontend == "realtime" { config::realtime_key(&c) } else { config::asr_key(&c) } } else { Some(input) }) else {
            w.set_voice_test_state(KEY_MISSING);
            w.set_voice_test_status(locale::pick("Enter a key for the selected voice provider (OpenAI can share the LLM key).", "请先填写所选语音服务的 key（OpenAI 可以和大模型共用一个 key）。").into());
            return;
        };
        let snapshot = voice_test_snapshot(&w);
        VOICE_TEST_INPUT.set(Some(snapshot.clone()));
        let epoch = w.get_voice_test_epoch().wrapping_add(1);
        w.set_voice_test_epoch(epoch);
        w.set_voice_test_state(KEY_TESTING);
        w.set_voice_test_status(if c.voice_frontend != "realtime" && c.asr_provider == "local" {
            locale::pick("Preparing local recognition. The first time, the language model is downloaded and verified (this can take a few minutes)…", "正在准备本地识别：第一次会下载并校验语音模型，可能需要几分钟…").into()
        } else {
            locale::pick("Checking the selected voice provider…", "正在验证所选语音服务…").into()
        });
        let weak = weak.clone();
        voice_rt.spawn(async move {
            let realtime_mode = c.voice_frontend == "realtime";
            let label = if realtime_mode { locale::pick("Realtime voice", "实时语音") } else { locale::pick("Speech recognition", "语音识别") };
            let result = tokio::time::timeout(Duration::from_secs(if c.asr_provider == "local" && !realtime_mode { 660 } else { 20 }), async {
                if realtime_mode {
                    let settings = realtime::RealtimeSettings {
                        provider: realtime::Provider::from_id(&c.realtime_provider), listen_only: true,
                        url: c.realtime_url.clone(), model: c.realtime_model.clone(),
                        voice: c.realtime_voice.clone(), api_key: key.clone(),
                        instructions: realtime::VOICE_PROMPT.to_string(), tools: Vec::new(),
                        speech_rate: (1.0 + c.tts_rate as f32 * 0.1).clamp(0.5, 2.0),
                    };
                    let session = realtime::Session::connect(&settings).await?;
                    // 不请求回复；配置确认后有界关闭测试会话。
                    let _ = tokio::time::timeout(Duration::from_secs(2), session.close()).await;
                    Ok::<String, anyhow::Error>(String::new())
                } else {
                    asr::check_detail(&asr::AsrSettings {
                        provider: asr::Provider::from_id(&c.asr_provider),
                        region: config::azure_region(&c),
                        local_model: c.local_asr_model.clone(),
                        url: c.asr_url.clone(), model: c.asr_model.clone(), api_key: key.clone(),
                        sentence_silence_ms: c.asr_sentence_silence_ms,
                        speech_timeout: Duration::from_secs(c.asr_speech_timeout_secs),
                    }).await
                }
            }).await;
            let (state, msg) = match result {
                Ok(Ok(answer)) => (KEY_OK, if answer.is_empty() { ui_format!("{label} connection test passed. Microphone and playback were not tested. Save & apply your changes.", "{label}连接测试通过；未测试麦克风拾音和声音播放。修改后请保存并应用。") } else { ui_format!("Server answered: {answer}", "服务器返回：{answer}") }),
                Ok(Err(e)) if c.asr_provider == "custom" && !realtime_mode => (KEY_FAILED,format!("{e:#}")),
                Ok(Err(_)) => (KEY_FAILED, ui_format!("{label} connection failed. Check credentials, model/voice access, and service URL, then retry.", "{label}连接失败，请检查凭证、模型/音色是否开通及服务地址，然后重试。")),
                Err(_) => (KEY_FAILED, ui_format!("{label} connection timed out. Check your network and service URL, then retry.", "{label}连接测试超时，请检查网络与服务地址后重试。")),
            };
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = weak.upgrade() {
                    if w.get_voice_test_epoch() == epoch {
                        if voice_test_snapshot(&w) == snapshot {
                            refresh_local_model_status(&w);
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
            w.set_asr_key_status(locale::pick("Paste a key above first", "请先在上面粘贴 key").into());
            return;
        };
        w.set_asr_key_state(KEY_TESTING);
        w.set_asr_key_status(locale::pick("Connecting…", "正在连接…").into());
        let weak = weak.clone();
        rt.spawn(async move {
            let settings = asr::AsrSettings {
                provider: asr::Provider::from_id(&c.asr_provider),
                        region: config::azure_region(&c),
                        local_model: c.local_asr_model.clone(),
                url: c.asr_url.clone(),
                model: c.asr_model.clone(),
                api_key: key,
                sentence_silence_ms: c.asr_sentence_silence_ms,
                speech_timeout: Duration::from_secs(c.asr_speech_timeout_secs),
            };
            let (state, msg) = match asr::check(&settings).await {
                Ok(()) => (KEY_OK, ui_format!("Test passed: {} connected", "测试通过：{} 可以正常连接", c.asr_model)),
                Err(e) => (KEY_FAILED, ui_format!("Test failed: {e:#}", "测试失败：{e:#}")),
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
    w.set_hotkey_label(if on && ok { locale::pick("Ctrl+Alt+Space", ball::HOTKEY_LABEL).into() } else { "".into() });
    w.set_hotkey_note(
        if !ok {
            ui_format!("{} is already used by another app", "{} 已被其他程序占用，没能启用", locale::pick("Ctrl+Alt+Space", ball::HOTKEY_LABEL))
        } else {
            ui_format!("Press {} to talk; press again while listening, thinking, or speaking to stop", "按 {} 开始说话；听、思考或回答时再按一下停止", locale::pick("Ctrl+Alt+Space", ball::HOTKEY_LABEL))
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
    let status = if locale::is_zh() {
        info.status
    } else {
        info.status.replace("没能打开：", "Could not start: ")
            .replace("电脑地址 ", "Computer address ")
            .replace("还没有配对手机", "No paired phones")
            .replace("已配对 ", "Paired phones: ")
            .replace(" 台手机，", "; online: ")
            .replace(" 台手机", "")
            .replace(" 台在线", "")
            .replace("正在打开…", "Starting…")
            .replace("打开开关并保存后，就可以配对手机", "Enable phone control and save to pair a phone")
    };
    w.set_remote_status(status.into());
}

thread_local! {
    static TRAY_TOGGLE: RefCell<Option<tray_icon::menu::MenuItem>> = const { RefCell::new(None) };
}

/// 悬浮球和托盘菜单里的“暂停 / 恢复语音唤醒”跟着当前状态走
fn sync_listening_menus(on: bool) {
    ball::set_listening(on);
    TRAY_TOGGLE.with_borrow(|item| {
        if let Some(item) = item {
            item.set_text(if on { locale::pick("Pause voice wake", "暂停语音唤醒") } else { locale::pick("Resume voice wake", "恢复语音唤醒") });
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
    let open = MenuItem::new(locale::pick("Open Aeri", "打开主界面"), true, None);
    let toggle = MenuItem::new(locale::pick("Pause voice wake", "暂停语音唤醒"), true, None);
    let show_ball = MenuItem::new(locale::pick("Show floating orb", "显示悬浮球"), true, None);
    let quit = MenuItem::new(locale::pick("Quit", "退出"), true, None);
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
        .with_tooltip(locale::pick("Aeri voice assistant", "Aeri 语音助手"))
        .with_icon(tray_icon::Icon::from_rgba(icon, 32, 32)?)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()?)
}

/// 托盘与 Windows 应用图标使用同一份品牌图形。
fn icon_rgba() -> Vec<u8> {
    include_bytes!("../assets/app-32.rgba").to_vec()
}
