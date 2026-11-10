//! 配置：保存在 %APPDATA%\VoiceAssistant\config.json，全部可以在界面“设置”页修改。
//! API key 用 Windows DPAPI 加密后保存，只有当前 Windows 用户能解密；也兼容系统环境变量。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptProtectData, CryptUnprotectData};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default = "Config::legacy_defaults")]
pub struct Config {
    // 唤醒
    pub wake_words: Vec<String>,
    /// 唤醒词检测阈值，越小越灵敏（也越容易误唤醒）
    pub kws_threshold: f32,
    /// 唤醒词加权，越大越容易被检出
    pub kws_score: f32,
    /// 麦克风名称（包含即可），空字符串表示系统默认
    pub input_device: String,
    pub listen_on_start: bool,
    pub show_window_on_start: bool,
    /// 回复后继续听几秒，不用再喊唤醒词
    pub follow_up: bool,
    /// 对话时把其他程序的声音调低，免得麦克风把电脑里的声音也听进去
    pub duck_others: bool,
    /// 每天自动检查一次 GitHub 上有没有新版本
    pub auto_update: bool,
    /// 对话和任务记录存到硬盘（保留 30 天）
    pub save_history: bool,

    // 云端流式识别（阿里云百炼 fun-asr-realtime）
    pub asr_provider: String,
    pub local_asr_model: String,
    pub stt_api_keys_protected: std::collections::BTreeMap<String, String>,
    pub asr_url: String,
    pub asr_model: String,
    /// DPAPI 加密后的 key（十六进制）
    pub asr_api_key_protected: String,
    /// 没在界面填 key 时，从这个环境变量读取
    pub asr_key_env: String,
    /// 说完多久算一句话结束（毫秒）
    pub asr_sentence_silence_ms: u32,
    /// 唤醒后多久没开口就放弃（秒）
    pub asr_speech_timeout_secs: u64,

    // 大模型（Anthropic Messages 格式，默认 DeepSeek）
    pub llm_provider: String,
    pub llm_key_provider: String,
    pub llm_api_keys_protected: std::collections::BTreeMap<String, String>,
    pub llm_base_url: String,
    pub llm_api_key_protected: String,
    pub llm_key_env: String,
    pub llm_model: String,
    pub llm_thinking: bool,
    pub llm_effort: String,
    pub new_conversation_after_secs: u64,

    // 后台智能体：复杂任务交给谁做
    /// codex / claude / builtin（内置 DeepSeek + 桌面/浏览器工具）
    pub executor: String,
    /// full（完全访问）/ workspace（只能改工作目录）/ readonly（只读）
    pub agent_access: String,
    /// 后台智能体的工作目录，空 = 用户目录
    pub agent_workdir: String,
    /// 后台智能体用的模型，空 = 它自己的默认设置
    pub agent_model: String,

    // 语音对话
    /// realtime（实时语音模型，边说边回、可打断）/ classic（识别 + 大模型 + 系统播报）
    pub voice_frontend: String,
    pub realtime_provider: String,
    pub realtime_openai_api_key_protected: String,
    pub realtime_url: String,
    pub realtime_model: String,
    pub realtime_voice: String,
    /// 播报时说唤醒词可以打断
    pub barge_in: bool,
    /// 戴耳机时可以开：播报时麦克风也一直开着，直接说话就能打断（外放会把自己的声音当成用户在说话）
    pub full_duplex: bool,
    /// Just start talking to interrupt a spoken reply (no wake word)
    pub talk_barge: bool,

    // 工具
    pub desktop_control: bool,
    pub browser_control: bool,
    pub browser_idle_minutes: u64,
    pub playwright_mcp: String,

    // 播报与外观
    pub tts_rate: i32,
    pub tts_provider: String,
    pub tts_voice: String,
    pub tts_model: String,
    pub azure_speech_region: String,
    /// auto（跟随系统）/ light / dark
    pub theme: String,
    /// Interface language: "system" (follow Windows), "en" or "zh"
    pub language: String,

    // 悬浮球
    pub float_ball: bool,
    /// 全局快捷键 Ctrl+Alt+空格：开始说话 / 停止
    pub hotkey: bool,
    /// 悬浮球上次的位置（物理像素）；i32::MIN 表示用默认位置
    pub ball_x: i32,
    pub ball_y: i32,

    // 手机控制（同一局域网里的安卓 App）
    pub remote_enabled: bool,
    /// WebSocket 和局域网发现（UDP）共用的端口
    pub remote_port: u16,

    /// 配置格式版本，用于一次性迁移旧配置（旧文件里没有这个字段，按 0 处理）
    #[serde(default = "legacy_version")]
    pub config_version: u32,
}

const CONFIG_VERSION: u32 = 2;

fn legacy_version() -> u32 {
    0
}

impl Config {
    fn legacy_defaults() -> Self {
        Self {
            // Defaults follow the interface language; saved wake words remain unchanged.
            wake_words: vec![crate::locale::pick("Hey Aeri", "你好电脑").into()],
            kws_threshold: 0.25,
            kws_score: 1.5,
            input_device: String::new(),
            listen_on_start: true,
            // 默认启动后只显示悬浮球，点开才出现主界面
            show_window_on_start: false,
            follow_up: true,
            duck_others: true,
            auto_update: true,
            save_history: true,
            asr_provider: "dashscope".into(),
            local_asr_model: "zipformer".into(),
            stt_api_keys_protected: Default::default(),
            asr_url: "wss://dashscope.aliyuncs.com/api-ws/v1/inference".into(),
            asr_model: "fun-asr-realtime".into(),
            asr_api_key_protected: String::new(),
            asr_key_env: "DASHSCOPE_API_KEY".into(),
            asr_sentence_silence_ms: 800,
            asr_speech_timeout_secs: 6,
            llm_provider: "deepseek".into(),
            llm_key_provider: "deepseek".into(),
            llm_api_keys_protected: Default::default(),
            llm_base_url: "https://api.deepseek.com/anthropic".into(),
            llm_api_key_protected: String::new(),
            llm_key_env: "DEEPSEEK_API_KEY".into(),
            llm_model: "deepseek-flash".into(),
            llm_thinking: true,
            llm_effort: "low".into(),
            new_conversation_after_secs: 300,
            executor: "codex".into(),
            agent_access: "full".into(),
            agent_workdir: String::new(),
            agent_model: String::new(),
            voice_frontend: "realtime".into(),
            realtime_provider: "dashscope".into(),
            realtime_openai_api_key_protected: String::new(),
            realtime_url: "wss://dashscope.aliyuncs.com/api-ws/v1/realtime".into(),
            realtime_model: "qwen-audio-3.1-realtime-plus".into(),
            realtime_voice: "longanqian_v3.1".into(),
            barge_in: true,
            full_duplex: false,
            talk_barge: true,
            desktop_control: true,
            browser_control: true,
            browser_idle_minutes: 5,
            playwright_mcp: "@playwright/mcp@0.0.83".into(),
            tts_rate: 1,
            tts_provider: "windows".into(),
            tts_voice: String::new(),
            tts_model: String::new(),
            azure_speech_region: String::new(),
            theme: "auto".into(),
            language: "system".into(),
            float_ball: true,
            hotkey: true,
            ball_x: i32::MIN,
            ball_y: i32::MIN,
            remote_enabled: false,
            remote_port: 47821,
            config_version: CONFIG_VERSION,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new_install(crate::locale::is_zh())
    }
}

impl Config {
    fn new_install(zh: bool) -> Self {
        let mut cfg = Self::legacy_defaults();
        cfg.voice_frontend = "classic".into();
        cfg.asr_provider = "local".into();
        if !zh {
            cfg.local_asr_model = "moonshine".into();
            let p = crate::llm::preset("openai");
            cfg.llm_provider = p.id.into();
            cfg.llm_key_provider = p.id.into();
            cfg.llm_base_url = p.url.into();
            cfg.llm_model = p.model.into();
            cfg.llm_key_env = p.env.into();
            cfg.llm_thinking = false;
            cfg.realtime_provider = "openai".into();
            cfg.realtime_url = "wss://api.openai.com/v1/realtime".into();
            cfg.realtime_model = "gpt-realtime-2".into();
            cfg.realtime_voice = "marin".into();
        }
        cfg
    }
}

pub fn app_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("VoiceAssistant")
}

/// No settings saved yet: this is the first run (the main window opens so the user can set things up)
pub fn first_run() -> bool {
    !config_path().exists()
}

fn config_path() -> PathBuf {
    app_dir().join("config.json")
}

pub fn load() -> Config {
    let mut cfg: Config = std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    cfg.wake_words = repair_wake_words(std::mem::take(&mut cfg.wake_words));
    cfg
}

/// Wake words typed in Settings: separated by commas or semicolons (Chinese ones too).
/// Spaces belong to the phrase, so "Hey Aeri" stays one wake word.
pub fn parse_wake_words(text: &str) -> Vec<String> {
    text.split([',', '，', '、', ';', '；'])
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Versions 0.4.0 and 0.4.1 also split on spaces and saved "Hey Aeri" as ["Hey", "Aeri"].
/// Join a lone English greeting with the English word that follows it.
fn repair_wake_words(words: Vec<String>) -> Vec<String> {
    const GREETINGS: [&str; 5] = ["hey", "hi", "hello", "ok", "okay"];
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let w = &words[i];
        let next = words.get(i + 1);
        let english = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphabetic());
        if GREETINGS.contains(&w.to_ascii_lowercase().as_str()) && next.is_some_and(|n| english(n) && !GREETINGS.contains(&n.to_ascii_lowercase().as_str())) {
            out.push(format!("{w} {}", next.unwrap()));
            i += 2;
        } else {
            out.push(w.clone());
            i += 1;
        }
    }
    out
}

impl Config {
    pub fn ball_position(&self) -> Option<(i32, i32)> {
        (self.ball_x != i32::MIN && self.ball_y != i32::MIN).then_some((self.ball_x, self.ball_y))
    }

    pub fn agent_dir(&self) -> PathBuf {
        let dir = self.agent_workdir.trim();
        if dir.is_empty() {
            std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
        } else {
            PathBuf::from(dir)
        }
    }
}

pub fn save(cfg: &Config) -> anyhow::Result<()> {
    std::fs::create_dir_all(app_dir())?;
    std::fs::write(config_path(), serde_json::to_string_pretty(cfg)?)?;
    Ok(())
}

/// 模型目录：优先环境变量 VA_MODELS，其次 exe 旁边的 models\，最后当前目录的 models\
pub fn models_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("VA_MODELS") {
        return PathBuf::from(dir);
    }
    let beside_exe = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("models")));
    match beside_exe {
        Some(dir) if dir.exists() => dir,
        _ => PathBuf::from("models"),
    }
}

// ---------- API key ----------

/// DPAPI 加密/解密（绑定当前 Windows 用户）
fn dpapi(data: &[u8], encrypt: bool) -> Option<Vec<u8>> {
    unsafe {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut output = CRYPT_INTEGER_BLOB::default();
        let result = if encrypt {
            CryptProtectData(&input, windows::core::w!("VoiceAssistant"), None, None, None, 0, &mut output)
        } else {
            CryptUnprotectData(&input, None, None, None, None, 0, &mut output)
        };
        result.ok()?;
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData as _)));
        Some(bytes)
    }
}

/// 加密 key，返回可以写进配置文件的十六进制字符串；失败返回 None
pub fn protect_key(plain: &str) -> Option<String> {
    dpapi(plain.trim().as_bytes(), true).map(|b| b.iter().map(|x| format!("{x:02x}")).collect())
}

pub(crate) fn unprotect_key(stored: &str) -> Option<String> {
    if stored.is_empty() || stored.len() % 2 != 0 {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..stored.len()).step_by(2).map(|i| u8::from_str_radix(&stored[i..i + 2], 16).ok()).collect();
    String::from_utf8(dpapi(&bytes?, false)?).ok().filter(|k| !k.is_empty())
}

/// 读取环境变量里的 key；从注册表兜底读取用户级变量（setx 设置后不用重启已打开的程序）
fn env_key(env_name: &str) -> Option<String> {
    if env_name.is_empty() {
        return None;
    }
    if let Ok(v) = std::env::var(env_name) {
        if !v.trim().is_empty() {
            return Some(v.trim().to_string());
        }
    }
    let out = std::process::Command::new("reg")
        .args(["query", r"HKCU\Environment", "/v", env_name])
        .creation_flags_no_window()
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find(|l| l.trim_start().starts_with(env_name))
        .and_then(|l| l.split_whitespace().last())
        .map(str::to_string)
}

/// 优先用界面里保存的 key，其次环境变量
pub fn llm_key(cfg: &Config) -> Option<String> {
    if cfg.llm_provider == "ollama" { return Some(String::new()); }
    unprotect_key(llm_protected(cfg)).or_else(|| shared_provider_key(cfg, &cfg.llm_provider)).or_else(|| env_key(&cfg.llm_key_env))
        .or_else(|| (cfg.llm_provider == "custom").then(String::new))
}

pub fn llm_protected(cfg: &Config) -> &str {
    if cfg.llm_provider == cfg.llm_key_provider { &cfg.llm_api_key_protected }
    else { cfg.llm_api_keys_protected.get(&cfg.llm_provider).map(String::as_str).unwrap_or("") }
}

pub fn llm_key_slot(cfg: &mut Config) -> &mut String {
    if cfg.llm_provider == cfg.llm_key_provider { &mut cfg.llm_api_key_protected }
    else { cfg.llm_api_keys_protected.entry(cfg.llm_provider.clone()).or_default() }
}

fn openai_key(cfg: &Config) -> Option<String> {
    let protected = if cfg.llm_key_provider == "openai" { &cfg.llm_api_key_protected }
        else { cfg.llm_api_keys_protected.get("openai").map(String::as_str).unwrap_or("") };
    unprotect_key(protected).or_else(|| env_key("OPENAI_API_KEY"))
}

// OpenAI voice shares the encrypted LLM key; other LLM providers can coexist
// with a separately saved voice key or OPENAI_API_KEY.
pub fn realtime_key(cfg: &Config) -> Option<String> {
    match cfg.realtime_provider.as_str() {
        "openai" => unprotect_key(&cfg.realtime_openai_api_key_protected).or_else(|| openai_key(cfg)),
        "azure" => realtime_slot_key(cfg, "AZURE_OPENAI_API_KEY"),
        "xai" => realtime_slot_key(cfg, "XAI_API_KEY"),
        // A local server may not need a key
        "custom" => realtime_slot_key(cfg, "").or_else(|| Some(String::new())),
        _ => unprotect_key(&cfg.asr_api_key_protected).or_else(|| env_key(&cfg.asr_key_env)),
    }
}

/// Realtime providers other than OpenAI and DashScope keep their key under "realtime-<id>"
fn realtime_slot(provider: &str) -> String {
    format!("realtime-{provider}")
}

fn realtime_slot_key(cfg: &Config, env: &str) -> Option<String> {
    cfg.stt_api_keys_protected
        .get(&realtime_slot(&cfg.realtime_provider))
        .and_then(|v| unprotect_key(v))
        .or_else(|| if env.is_empty() { None } else { env_key(env) })
}

pub fn asr_key(cfg: &Config) -> Option<String> {
    match cfg.asr_provider.as_str() {
        "local" => Some(String::new()),
        "custom" => shared_provider_key(cfg, "custom").or_else(|| Some(String::new())),
        "deepgram" | "elevenlabs" | "azure" | "gemini" => shared_provider_key(cfg, &cfg.asr_provider),
        "openai" => shared_provider_key(cfg,"openai"),
        "groq" => shared_provider_key(cfg,"groq"),
        _ => unprotect_key(&cfg.asr_api_key_protected).or_else(|| env_key(&cfg.asr_key_env)),
    }
}

pub fn voice_key_slot(cfg: &mut Config) -> &mut String {
    if cfg.voice_frontend == "realtime" {
        match cfg.realtime_provider.as_str() {
            "openai" => &mut cfg.realtime_openai_api_key_protected,
            "dashscope" => &mut cfg.asr_api_key_protected,
            other => {
                let slot = realtime_slot(other);
                cfg.stt_api_keys_protected.entry(slot).or_default()
            }
        }
    } else if cfg.asr_provider != "dashscope" {
        cfg.stt_api_keys_protected.entry(cfg.asr_provider.clone()).or_default()
    } else { &mut cfg.asr_api_key_protected }
}

// Speech keys are shared across recognition and speech output, with fallback to
// the same provider's LLM slot. Existing OpenAI/DashScope slots stay readable.
pub fn shared_provider_key(cfg: &Config, provider: &str) -> Option<String> {
    cfg.stt_api_keys_protected.get(provider).and_then(|v| unprotect_key(v))
        .or_else(|| {
            let stored = if cfg.llm_key_provider == provider { &cfg.llm_api_key_protected }
                else { cfg.llm_api_keys_protected.get(provider).map(String::as_str).unwrap_or("") };
            unprotect_key(stored)
        })
        .or_else(|| if provider == "openai" { unprotect_key(&cfg.realtime_openai_api_key_protected) } else { None })
        .or_else(|| env_key(match provider {
            "openai" => "OPENAI_API_KEY", "groq" => "GROQ_API_KEY", "deepgram" => "DEEPGRAM_API_KEY",
            "elevenlabs" => "ELEVENLABS_API_KEY", "azure" => "AZURE_SPEECH_KEY", "gemini" => "GEMINI_API_KEY", _ => "",
        }))
        .or_else(|| if provider == "gemini" { env_key("GOOGLE_API_KEY") } else { None })
}

pub fn tts_key(cfg: &Config) -> Option<String> {
    if cfg.tts_provider == "windows" { Some(String::new()) } else { shared_provider_key(cfg, &cfg.tts_provider) }
}

pub fn azure_region(cfg: &Config) -> String {
    if cfg.azure_speech_region.trim().is_empty() { env_key("AZURE_SPEECH_REGION").unwrap_or_default() }
    else { cfg.azure_speech_region.trim().to_string() }
}

pub fn voice_protected(cfg: &Config) -> &str {
    if cfg.voice_frontend == "realtime" {
        match cfg.realtime_provider.as_str() {
            "openai" => &cfg.realtime_openai_api_key_protected,
            "dashscope" => &cfg.asr_api_key_protected,
            other => cfg.stt_api_keys_protected.get(&realtime_slot(other)).map(String::as_str).unwrap_or(""),
        }
    } else if cfg.asr_provider != "dashscope" {
        cfg.stt_api_keys_protected.get(&cfg.asr_provider).map(String::as_str).unwrap_or("")
    } else { &cfg.asr_api_key_protected }
}

/// 给界面显示的 key 状态（不显示 key 本身）
/// 界面里保存过 key（不算环境变量）
pub fn has_saved_key(protected: &str) -> bool {
    unprotect_key(protected).is_some()
}

pub fn key_status(protected: &str, env_name: &str) -> String {
    if unprotect_key(protected).is_some() {
        "已保存（加密存放在本机）".into()
    } else if env_key(env_name).is_some() {
        format!("正在使用系统环境变量 {env_name}")
    } else {
        "未设置".into()
    }
}

pub trait NoWindow {
    fn creation_flags_no_window(&mut self) -> &mut Self;
}

impl NoWindow for std::process::Command {
    fn creation_flags_no_window(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        self.creation_flags(0x0800_0000) // CREATE_NO_WINDOW：不弹黑框
    }
}

impl NoWindow for tokio::process::Command {
    fn creation_flags_no_window(&mut self) -> &mut Self {
        self.creation_flags(0x0800_0000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_and_new_install_defaults_are_separate() {
        let old: Config = serde_json::from_str(r#"{"llm_model":"my-model","show_window_on_start":true}"#).unwrap();
        assert_eq!(old.llm_provider, "deepseek");
        assert_eq!(old.voice_frontend, "realtime");
        assert_eq!(old.asr_provider, "dashscope");
        assert_eq!(old.realtime_provider, "dashscope");
        assert_eq!(old.llm_model, "my-model");
        assert!(old.show_window_on_start);
        let en = Config::new_install(false);
        assert_eq!(en.llm_provider, "openai");
        assert_eq!(en.voice_frontend, "classic");
        assert_eq!(en.asr_provider, "local");
        assert_eq!(en.realtime_provider, "openai");
        assert_eq!(en.llm_key_env, "OPENAI_API_KEY");
        let zh = Config::new_install(true);
        assert_eq!(zh.llm_provider, "deepseek");
        assert_eq!(zh.realtime_provider, "dashscope");
        let roundtrip: Config = serde_json::from_value(serde_json::to_value(&en).unwrap()).unwrap();
        assert_eq!(roundtrip.llm_model, en.llm_model);
        assert_eq!(roundtrip.realtime_url, en.realtime_url);
    }

    #[test]
    fn keys_are_scoped_to_provider_and_openai_voice_can_share() {
        let mut cfg = Config::new_install(false);
        cfg.llm_api_key_protected = protect_key("openai-example").unwrap();
        assert_eq!(realtime_key(&cfg).as_deref(), Some("openai-example"));
        cfg.llm_provider = "groq".into();
        cfg.llm_key_env.clear();
        assert_eq!(llm_key(&cfg), None);
        *llm_key_slot(&mut cfg) = protect_key("groq-example").unwrap();
        assert_eq!(llm_key(&cfg).as_deref(), Some("groq-example"));
        assert_eq!(realtime_key(&cfg).as_deref(), Some("openai-example"));
        cfg.llm_provider = "openai".into();
        assert_eq!(llm_key(&cfg).as_deref(), Some("openai-example"));
        cfg.asr_provider = "openai".into();
        assert_eq!(asr_key(&cfg).as_deref(), Some("openai-example"));
    }

    #[test]
    fn speech_keys_share_provider_slots_without_changing_legacy_defaults() {
        let old: Config = serde_json::from_str(r#"{"asr_model":"saved-model"}"#).unwrap();
        assert_eq!(old.tts_provider,"windows");
        assert_eq!(old.local_asr_model,"zipformer");
        assert_eq!(old.asr_model,"saved-model");
        assert_eq!(Config::new_install(false).local_asr_model,"moonshine");
        assert_eq!(Config::new_install(true).local_asr_model,"zipformer");
        let mut cfg = Config::default();
        cfg.voice_frontend = "classic".into();
        cfg.asr_provider = "elevenlabs".into();
        cfg.tts_provider = "elevenlabs".into();
        *voice_key_slot(&mut cfg) = protect_key("sample-eleven-key").unwrap();
        assert_eq!(asr_key(&cfg).as_deref(),Some("sample-eleven-key"));
        assert_eq!(tts_key(&cfg),asr_key(&cfg));
        cfg.llm_provider = "gemini".into();
        *llm_key_slot(&mut cfg) = protect_key("sample-google-key").unwrap();
        cfg.asr_provider = "gemini".into();
        assert_eq!(asr_key(&cfg).as_deref(),Some("sample-google-key"));
        cfg.asr_provider = "custom".into();
        assert_eq!(asr_key(&cfg),Some(String::new()));
    }

    #[test]
    fn key_roundtrip() {
        let stored = protect_key("  sk-test-123  ").unwrap();
        assert!(!stored.contains("sk-test")); // 文件里看不到明文
        assert_eq!(unprotect_key(&stored).as_deref(), Some("sk-test-123"));
        assert_eq!(unprotect_key("zz"), None);
        assert_eq!(unprotect_key(""), None);
    }
}

#[cfg(test)]
mod wake_word_tests {
    use super::{parse_wake_words, repair_wake_words};

    #[test]
    fn spaces_stay_inside_a_wake_word() {
        assert_eq!(parse_wake_words("Hey Aeri"), vec!["Hey Aeri"]);
        assert_eq!(parse_wake_words(" Hey   Aeri , 你好电脑，小爱同学"), vec!["Hey Aeri", "你好电脑", "小爱同学"]);
        assert!(parse_wake_words(" , ").is_empty());
    }

    #[test]
    fn repairs_words_split_by_older_versions() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(repair_wake_words(v(&["Hey", "Aeri"])), v(&["Hey Aeri"]));
        assert_eq!(repair_wake_words(v(&["你好电脑", "Hey", "Aeri"])), v(&["你好电脑", "Hey Aeri"]));
        assert_eq!(repair_wake_words(v(&["Hey Aeri"])), v(&["Hey Aeri"]));
        assert_eq!(repair_wake_words(v(&["你好电脑", "小爱同学"])), v(&["你好电脑", "小爱同学"]));
        assert_eq!(repair_wake_words(v(&["Hey"])), v(&["Hey"]));
    }
}
