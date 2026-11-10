//! 常用命令直达：打开/关闭应用、搜索、问时间、调音量、切歌，不经过大模型。
//! 只有在很确定的时候才处理，否则返回 None 交给大模型。

use std::sync::LazyLock;

use regex::Regex;

use crate::sys::{self, MediaKey};
use crate::tools::apps::{self, AppIndex, Match};
use crate::tools::process::{self, Closed};
use crate::tools::system;

static OPEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:打开|启动|运行|开一下|开)(?:一下)?(.+?)(?:一下|吧)?$").unwrap());
// “打开抖音搜索无畏契约直播”“在B站上搜一下原神”“淘宝搜机械键盘”
static SITE_SEARCH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:打开|在|去|用|到)?(.{1,8}?)(?:上面|里面|上|里|中)?[，, ]*(?:然后)?(?:搜索|搜一下|搜一搜|搜|查找|找一下|找)(.+)$").unwrap()
});
// “关闭微信”“关掉 GPT 的后台”“把网易云音乐关了”
static CLOSE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:关闭|关掉|关了|退出|结束)(?:一下)?(.+?)(?:的?(?:后台|进程|程序))?吧?$").unwrap());
static CLOSE_BA: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^把(.+?)(?:的?(?:后台|进程|程序))?给?(?:关闭|关掉|关了|退出|结束掉?)吧?$").unwrap()
});
static SEARCH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:搜索|搜一下|搜一搜|百度一下|百度|必应一下)(?:一下)?(.+)$").unwrap());
// “音量调到 30”“声音调成百分之五十”“把音量设为 80%”
static VOLUME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:把)?(?:音量|声音)(?:调到|调成|设为|设成|设置为|设置成|开到)(?:百分之)?(\d{1,3}|[零一二两三四五六七八九十百]+)%?$").unwrap()
});
static TIME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:现在)?(?:几点了?|什么时间|几号|今天几号|今天星期几|星期几)$").unwrap());

/// “30”“三十”“一百”“五十五” → 数字
fn parse_number(s: &str) -> Option<u32> {
    if let Ok(n) = s.parse() {
        return Some(n);
    }
    let digit = |c: char| "零一二三四五六七八九".find(c).map(|i| (i / 3) as u32).or((c == '两').then_some(2));
    if s == "一百" || s == "百" {
        return Some(100);
    }
    let chars: Vec<char> = s.chars().collect();
    match chars.as_slice() {
        ['十'] => Some(10),
        [d] => digit(*d),
        ['十', d] => Some(10 + digit(*d)?),
        [d, '十'] => Some(digit(*d)? * 10),
        [a, '十', b] => Some(digit(*a)? * 10 + digit(*b)?),
        _ => None,
    }
}

fn normalize(text: &str) -> String {
    let mut t = text.trim().trim_matches(|c: char| "。.！!？?，, 、~".contains(c)).to_string();
    for prefix in ["请你", "请", "帮我", "麻烦你", "麻烦", "给我"] {
        if let Some(rest) = t.strip_prefix(prefix) {
            t = rest.to_string();
        }
    }
    t.trim().to_string()
}

pub fn try_handle(text: &str, apps: &AppIndex) -> Option<String> {
    if crate::locale::is_zh() { try_handle_zh(text, apps) } else { try_handle_en(text, apps) }
}

fn try_handle_zh(text: &str, apps: &AppIndex) -> Option<String> {
    let t = normalize(text);
    if t.is_empty() || t.chars().count() > 20 {
        return None;
    }

    if TIME.is_match(&t) {
        return Some(format!("现在是{}。", sys::now_text()));
    }

    let media = match t.as_str() {
        "大声点" | "大声一点" | "音量调大" | "音量调大一点" | "声音大一点" | "调大音量" => Some((MediaKey::VolumeUp, 5, "音量已调大。")),
        "小声点" | "小声一点" | "音量调小" | "音量调小一点" | "声音小一点" | "调小音量" => Some((MediaKey::VolumeDown, 5, "音量已调小。")),
        "静音" | "取消静音" => Some((MediaKey::Mute, 1, "好的。")),
        "暂停" | "暂停播放" | "继续播放" | "播放" => Some((MediaKey::PlayPause, 1, "好的。")),
        "下一首" | "下一曲" | "切歌" => Some((MediaKey::Next, 1, "下一首。")),
        "上一首" | "上一曲" => Some((MediaKey::Previous, 1, "上一首。")),
        _ => None,
    };
    if let Some(c) = VOLUME.captures(&t) {
        if let Some(p) = parse_number(&c[1]).filter(|p| *p <= 100) {
            if system::set_volume(Some(p), None).is_ok() {
                return Some(format!("音量已调到{p}。"));
            }
        }
    }

    match t.as_str() {
        "锁屏" | "锁定电脑" | "锁定屏幕" | "锁一下屏幕" => {
            let _ = unsafe { windows::Win32::System::Shutdown::LockWorkStation() };
            return Some("已锁屏。".into());
        }
        "显示桌面" | "回到桌面" | "最小化所有窗口" => {
            system::show_desktop();
            return Some("好的。".into());
        }
        _ => {}
    }

    if let Some((key, times, reply)) = media {
        sys::press_media_key(key, times);
        return Some(reply.into());
    }

    // 要在“打开 XX”之前判断，否则“打开抖音搜索 XX”会被当成打开一个叫“抖音搜索 XX”的程序
    if let Some(c) = SITE_SEARCH.captures(&t) {
        if let Some(engine) = apps::site_engine(&c[1]) {
            let query = c[2].trim().trim_start_matches(['一', '下']).trim();
            if !query.is_empty() && sys::shell_open(&apps::search_url(query, engine)) {
                return Some(format!("已在{}搜索{query}。", c[1].trim()));
            }
        }
    }

    if let Some(c) = CLOSE.captures(&t).or_else(|| CLOSE_BA.captures(&t)) {
        let name = c[1].trim();
        // 只处理和进程名或开始菜单应用名完全一致的，其余（关闭蓝牙、关掉这个网页…）交给大模型
        // “关闭当前直播”“关掉这个网页”指的是屏幕上的东西，要结合前台窗口判断，也交给大模型
        let deictic = ["当前", "这个", "这", "那个", "正在"].iter().any(|p| name.starts_with(p));
        if !name.contains('的') && !deictic {
            if let Closed::Done(text) = process::close(name, apps, true) {
                return Some(if text.starts_with("已关闭") { format!("已关闭{name}。") } else { format!("{text}。") });
            }
        }
        return None;
    }

    if let Some(c) = OPEN.captures(&t) {
        let name = c[1].trim();
        // “打开XX的YY”“打开网页搜索”之类复杂请求交给大模型
        if !name.contains('的') && !name.contains("网站") && !name.contains("网页") {
            if let Match::One(app) = apps.best(name) {
                if apps::launch(&app) {
                    return Some(format!("已打开{}。", app.name));
                }
            }
        }
        return None;
    }

    if let Some(c) = SEARCH.captures(&t) {
        let query = c[1].trim();
        if !query.is_empty() && sys::shell_open(&apps::search_url(query, "bing")) {
            return Some(format!("已为你搜索{query}。"));
        }
    }
    None
}

#[derive(Debug, PartialEq)]
enum EnglishCommand {
    Open(String),
    Close(String),
    Search(String, String),
    Volume(u32),
    Media(&'static str),
    Lock,
    Desktop,
    Time,
}

static EN_OPEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:open|launch|start) (.+)$").unwrap());
static EN_CLOSE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:close|quit|exit) (.+)$").unwrap());
static EN_SEARCH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^search (.+?)(?: on (youtube|google|amazon|reddit|x|twitter|wikipedia|spotify|bing|github))?$").unwrap());
static EN_VOLUME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:set (?:the )?)?volume(?: to)? (\d{1,3})(?:\s*(?:%|percent))?$").unwrap());

/// Parse without side effects so language rules can be tested independently of Windows.
fn english_command(text: &str) -> Option<EnglishCommand> {
    let mut t = text.trim().trim_end_matches(['.', '!', '?']).trim();
    if t.len() > 240 { return None; }
    if t.get(..7).is_some_and(|s| s.eq_ignore_ascii_case("please ")) { t = t[7..].trim(); }
    if t.len() >= 7 && t.get(t.len()-7..).is_some_and(|s| s.eq_ignore_ascii_case(" please")) { t = t[..t.len()-7].trim(); }
    let lower = t.to_ascii_lowercase();
    match lower.as_str() {
        "what time is it" | "what's the time" | "what is the time" | "what is today's date" | "what day is it" => return Some(EnglishCommand::Time),
        "lock the screen" | "lock screen" | "lock the computer" | "lock my computer" => return Some(EnglishCommand::Lock),
        "show desktop" | "show the desktop" | "minimize all windows" => return Some(EnglishCommand::Desktop),
        "mute" | "mute the sound" => return Some(EnglishCommand::Media("mute")),
        "unmute" | "unmute the sound" => return Some(EnglishCommand::Media("unmute")),
        "volume up" | "turn up the volume" | "louder" => return Some(EnglishCommand::Media("volume_up")),
        "volume down" | "turn down the volume" | "quieter" => return Some(EnglishCommand::Media("volume_down")),
        "pause" | "pause playback" | "play" | "resume playback" => return Some(EnglishCommand::Media("play_pause")),
        "next song" | "next track" | "skip track" => return Some(EnglishCommand::Media("next")),
        "previous song" | "previous track" => return Some(EnglishCommand::Media("previous")),
        _ => {}
    }
    if let Some(c) = EN_VOLUME.captures(t) {
        return c[1].parse::<u32>().ok().filter(|p| *p <= 100).map(EnglishCommand::Volume);
    }
    if let Some(c) = EN_SEARCH.captures(t) {
        return Some(EnglishCommand::Search(c[1].trim().into(), c.get(2).and_then(|s| apps::site_engine(s.as_str())).unwrap_or("google").into()));
    }
    if let Some(c) = EN_CLOSE.captures(t) {
        let name = c[1].trim();
        if ["this", "that", "current", "the current", "the window", "the tab"].iter().any(|p| name.to_ascii_lowercase().starts_with(p)) { return None; }
        return Some(EnglishCommand::Close(name.into()));
    }
    if let Some(c) = EN_OPEN.captures(t) {
        let name = c[1].trim();
        if name.contains(" and ") || name.contains(" then ") { return None; }
        return Some(EnglishCommand::Open(name.into()));
    }
    None
}

fn try_handle_en(text: &str, apps: &AppIndex) -> Option<String> {
    match english_command(text)? {
        EnglishCommand::Time => {
            let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
            Some(format!("It is {:02}:{:02}, {}-{:02}-{:02}.", t.wHour, t.wMinute, t.wYear, t.wMonth, t.wDay))
        }
        EnglishCommand::Lock => {
            unsafe { windows::Win32::System::Shutdown::LockWorkStation() }.ok()?;
            Some("Screen locked.".into())
        }
        EnglishCommand::Desktop => { system::show_desktop(); Some("Done.".into()) }
        EnglishCommand::Volume(percent) => {
            system::set_volume(Some(percent), None).ok()?;
            Some(format!("Volume set to {percent} percent."))
        }
        EnglishCommand::Media(action) => {
            let (key, times, reply) = match action {
                "mute" | "unmute" => {
                    system::set_volume(None, Some(action == "mute")).ok()?;
                    return Some(if action == "mute" { "Muted." } else { "Unmuted." }.into());
                }
                "volume_up" => (MediaKey::VolumeUp, 5, "Volume increased."),
                "volume_down" => (MediaKey::VolumeDown, 5, "Volume decreased."),
                "next" => (MediaKey::Next, 1, "Next track."),
                "previous" => (MediaKey::Previous, 1, "Previous track."),
                _ => (MediaKey::PlayPause, 1, "Done."),
            };
            sys::press_media_key(key, times);
            Some(reply.into())
        }
        EnglishCommand::Search(query, engine) => {
            sys::shell_open(&apps::search_url(&query, &engine)).then(|| format!("Searched {engine} for {query}."))
        }
        EnglishCommand::Open(name) => {
            if let Match::One(app) = apps.best(&name) {
                if apps::launch(&app) { return Some(format!("Opened {}.", app.name)); }
            }
            None
        }
        EnglishCommand::Close(name) => match process::close(&name, apps, true) {
            Closed::Done(text) => Some(format!("{text}.")),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_phrases() {
        for (t, n) in [("音量调到30", "30"), ("把音量调到百分之五十", "五十"), ("声音调成80%", "80")] {
            assert_eq!(&VOLUME.captures(t).unwrap()[1], n);
        }
        assert_eq!(parse_number("五十"), Some(50));
        assert_eq!(parse_number("三十五"), Some(35));
        assert_eq!(parse_number("十五"), Some(15));
        assert_eq!(parse_number("一百"), Some(100));
        assert_eq!(parse_number("两"), Some(2));
        assert!(!VOLUME.is_match("音量调大"));
    }

    #[test]
    fn close_phrases() {
        let name = |text: &str| {
            let t = normalize(text);
            CLOSE.captures(&t).or_else(|| CLOSE_BA.captures(&t)).map(|c| c[1].to_string())
        };
        assert_eq!(name("关闭GPT的后台。").as_deref(), Some("GPT"));
        assert_eq!(name("帮我关掉微信吧").as_deref(), Some("微信"));
        assert_eq!(name("把网易云音乐关了").as_deref(), Some("网易云音乐"));
        assert_eq!(name("结束ChatGPT进程").as_deref(), Some("ChatGPT"));
        assert_eq!(name("打开微信"), None);
    }

    #[test]
    fn deictic_close_goes_to_model() {
        let apps = AppIndex::load_cached();
        assert_eq!(try_handle_zh("关闭当前直播", &apps), None);
        assert_eq!(try_handle_zh("把这个网页关掉", &apps), None);
    }

    #[test]
    fn site_search_phrases() {
        let parse = |text: &str| {
            let t = normalize(text);
            SITE_SEARCH
                .captures(&t)
                .and_then(|c| apps::site_engine(&c[1]).map(|e| (e, c[2].trim().trim_start_matches(['一', '下']).trim().to_string())))
        };
        assert_eq!(parse("打开抖音搜索无畏契约直播"), Some(("douyin", "无畏契约直播".into())));
        assert_eq!(parse("帮我在抖音里面搜一下无畏契约直播。"), Some(("douyin", "无畏契约直播".into())));
        assert_eq!(parse("打开抖音，然后搜索无畏契约直播"), Some(("douyin", "无畏契约直播".into())));
        assert_eq!(parse("在B站上搜原神"), Some(("bilibili", "原神".into())));
        assert_eq!(parse("淘宝搜机械键盘"), Some(("taobao", "机械键盘".into())));
        assert_eq!(parse("搜索明天的天气"), None); // 没说网站，交给普通搜索
        assert_eq!(parse("打开记事本"), None);
    }

    #[test]
    fn english_phrases() {
        assert_eq!(english_command("Please launch Calculator!"), Some(EnglishCommand::Open("Calculator".into())));
        assert_eq!(english_command("quit Notepad please."), Some(EnglishCommand::Close("Notepad".into())));
        assert_eq!(english_command("set the volume to 30 percent"), Some(EnglishCommand::Volume(30)));
        assert_eq!(english_command("volume to 101"), None);
        assert_eq!(english_command("mute"), Some(EnglishCommand::Media("mute")));
        assert_eq!(english_command("unmute"), Some(EnglishCommand::Media("unmute")));
        assert_eq!(english_command("lock the screen"), Some(EnglishCommand::Lock));
        assert_eq!(english_command("show desktop"), Some(EnglishCommand::Desktop));
        assert_eq!(english_command("what time is it?"), Some(EnglishCommand::Time));
        for site in ["YouTube", "Google", "Amazon", "Reddit", "Spotify", "Wikipedia"] {
            assert_eq!(english_command(&format!("search Rust tutorials on {site}")), Some(EnglishCommand::Search("Rust tutorials".into(), site.to_lowercase())));
        }
        assert_eq!(english_command("search weather tomorrow"), Some(EnglishCommand::Search("weather tomorrow".into(), "google".into())));
        assert_eq!(english_command("close this window"), None);
        assert_eq!(english_command("open Calculator and type 5"), None);
        assert_eq!(english_command("打开记事本"), None);
        assert_eq!(english_command("搜索天气"), None);
        assert_eq!(english_command("请问现在几时"), None);
    }

    #[test]
    fn dispatches_only_the_selected_language() {
        let apps = AppIndex::load_cached();
        assert_eq!(try_handle("what time is it", &apps).is_some(), !crate::locale::is_zh());
        assert_eq!(try_handle("现在几点", &apps).is_some(), crate::locale::is_zh());
    }
}
