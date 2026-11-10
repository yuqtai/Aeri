//! 本地工具：启动程序（开始菜单）、默认浏览器搜索/打开网址、时间、音量和播放控制。

use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::{self, NoWindow};
use crate::sys::{self, MediaKey};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct App {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "AppID")]
    pub id: String,
}

pub enum Match {
    One(App),
    Many(Vec<App>),
    None,
}

/// 开始菜单应用索引：先读缓存立即可用，后台再用 Get-StartApps 刷新
pub struct AppIndex {
    apps: RwLock<Vec<App>>,
}

impl AppIndex {
    pub fn load_cached() -> Self {
        let apps = std::fs::read_to_string(config::app_dir().join("apps.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { apps: RwLock::new(apps) }
    }

    /// 调用 PowerShell 的 Get-StartApps 重新扫描（约 1~2 秒，放后台线程跑）
    pub fn refresh(&self) {
        let out = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "[Console]::OutputEncoding=[Text.Encoding]::UTF8; Get-StartApps | ConvertTo-Json -Compress",
            ])
            .creation_flags_no_window()
            .output();
        let Ok(out) = out else { return };
        let Ok(mut apps) = serde_json::from_slice::<Vec<App>>(&out.stdout) else { return };
        apps.retain(|a| !a.name.contains("卸载") && !a.name.to_lowercase().contains("uninstall"));
        let _ = std::fs::create_dir_all(config::app_dir());
        let _ = std::fs::write(config::app_dir().join("apps.json"), serde_json::to_string(&apps).unwrap_or_default());
        *self.apps.write().unwrap() = apps;
    }

    fn search(&self, keyword: &str) -> Vec<App> {
        let norm = |s: &str| s.to_lowercase().replace(' ', "");
        let k = norm(keyword);
        if k.is_empty() {
            return Vec::new();
        }
        let apps = self.apps.read().unwrap();
        let (mut exact, mut prefix, mut contains) = (Vec::new(), Vec::new(), Vec::new());
        for app in apps.iter() {
            let n = norm(&app.name);
            if n == k {
                exact.push(app.clone());
            } else if n.starts_with(&k) {
                prefix.push(app.clone());
            } else if n.contains(&k) {
                contains.push(app.clone());
            }
        }
        exact.into_iter().chain(prefix).chain(contains).collect()
    }

    /// 完全同名或只有一个候选时才算确定
    pub fn best(&self, name: &str) -> Match {
        let hits = self.search(name);
        let lower = name.to_lowercase().replace(' ', "");
        if let Some(app) = hits.iter().find(|a| a.name.to_lowercase().replace(' ', "") == lower) {
            return Match::One(app.clone());
        }
        match hits.len() {
            0 => Match::None,
            1 => Match::One(hits[0].clone()),
            _ => Match::Many(hits),
        }
    }
}

pub fn launch(app: &App) -> bool {
    std::process::Command::new("explorer.exe").arg(format!("shell:AppsFolder\\{}", app.id)).spawn().is_ok()
}

pub fn search_url(query: &str, engine: &str) -> String {
    let q: String = query
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    match engine {
        "baidu" => format!("https://www.baidu.com/s?wd={q}"),
        "google" => format!("https://www.google.com/search?q={q}"),
        "bilibili" => format!("https://search.bilibili.com/all?keyword={q}"),
        "zhihu" => format!("https://www.zhihu.com/search?q={q}"),
        "douyin" => format!("https://www.douyin.com/search/{q}"),
        "xiaohongshu" => format!("https://www.xiaohongshu.com/search_result?keyword={q}"),
        "taobao" => format!("https://s.taobao.com/search?q={q}"),
        "jd" => format!("https://search.jd.com/Search?keyword={q}"),
        "youtube" => format!("https://www.youtube.com/results?search_query={q}"),
        "github" => format!("https://github.com/search?q={q}"),
        "amazon" => format!("https://www.amazon.com/s?k={q}"),
        "reddit" => format!("https://www.reddit.com/search/?q={q}"),
        "x" => format!("https://x.com/search?q={q}"),
        "wikipedia" => format!("https://en.wikipedia.org/w/index.php?search={q}"),
        "spotify" => format!("https://open.spotify.com/search/{q}"),
        _ if !crate::locale::is_zh() => format!("https://www.google.com/search?q={q}"),
        _ => format!("https://www.bing.com/search?q={q}"),
    }
}

/// 口语里的网站名 → web_search 的 engine
pub fn site_engine(name: &str) -> Option<&'static str> {
    let n = name.trim().to_lowercase().replace(' ', "");
    Some(match n.as_str() {
        "抖音" | "douyin" => "douyin",
        "b站" | "哔哩哔哩" | "bilibili" | "b 站" => "bilibili",
        "小红书" => "xiaohongshu",
        "淘宝" => "taobao",
        "京东" => "jd",
        "知乎" => "zhihu",
        "百度" => "baidu",
        "必应" | "bing" => "bing",
        "谷歌" | "google" => "google",
        "youtube" | "油管" => "youtube",
        "github" => "github",
        "amazon" | "亚马逊" => "amazon",
        "reddit" => "reddit",
        "x" | "twitter" | "推特" => "x",
        "wikipedia" | "维基百科" => "wikipedia",
        "spotify" => "spotify",
        _ => return None,
    })
}

pub fn definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "launch_app",
            "description": "Launch an installed Start menu app, such as Notepad, Calculator, WeChat or Microsoft Edge. Use find_apps first if unsure of its name.",
            "input_schema": {"type": "object", "properties": {"name": {"type": "string", "description": "Application name, preferably its full Start menu name."}}, "required": ["name"]}
        }),
        json!({
            "name": "find_apps",
            "description": "Find installed application names by a fuzzy keyword search.",
            "input_schema": {"type": "object", "properties": {"keyword": {"type": "string"}}, "required": ["keyword"]}
        }),
        json!({
            "name": "web_search",
            "description": "Open a site's search results in the user's default browser with their signed-in session, in one step. This does not read page content. Choose the requested site engine (including Google, Amazon, Reddit, X, Wikipedia, Spotify and the Chinese sites). Without a specified site, omit engine: English defaults to google, Chinese to bing.",
            "input_schema": {"type": "object", "properties": {
                "query": {"type": "string"},
                "engine": {"type": "string", "enum": ["bing", "baidu", "google", "bilibili", "zhihu", "douyin", "xiaohongshu", "taobao", "jd", "youtube", "github", "amazon", "reddit", "x", "wikipedia", "spotify"]}
            }, "required": ["query"]}
        }),
        json!({
            "name": "open_url",
            "description": "Open a URL in the user's default browser. This does not read page content.",
            "input_schema": {"type": "object", "properties": {"url": {"type": "string"}}, "required": ["url"]}
        }),
        json!({
            "name": "current_time",
            "description": "Get the current local date, day of week and time.",
            "input_schema": {"type": "object", "properties": {}}
        }),
        json!({
            "name": "media_control",
            "description": "Adjust system volume or control media playback.",
            "input_schema": {"type": "object", "properties": {
                "action": {"type": "string", "enum": ["volume_up", "volume_down", "mute", "play_pause", "next", "previous"]},
                "times": {"type": "integer", "description": "Number of key presses; each volume press changes about 2 percent; default 5."}
            }, "required": ["action"]}
        }),
    ]
}

/// 执行本地工具；不是本地工具时返回 None
pub fn call(index: &AppIndex, name: &str, input: &Value) -> Option<(String, bool)> {
    let s = |k: &str| input[k].as_str().unwrap_or("").trim().to_string();
    Some(match name {
        "launch_app" => match index.best(&s("name")) {
            Match::One(app) if launch(&app) => (format!("已启动：{}", app.name), false),
            Match::One(app) => (format!("启动失败：{}", app.name), true),
            Match::Many(apps) => (
                format!("有多个候选，请用完整名称再调用：{}", apps.iter().take(10).map(|a| a.name.as_str()).collect::<Vec<_>>().join("、")),
                true,
            ),
            Match::None => (format!("没有找到应用“{}”，可以先用 find_apps 换个关键词查找。", s("name")), true),
        },
        "find_apps" => {
            let hits = index.search(&s("keyword"));
            if hits.is_empty() {
                (format!("没有找到包含“{}”的应用", s("keyword")), false)
            } else {
                (format!("匹配到：{}", hits.iter().take(15).map(|a| a.name.as_str()).collect::<Vec<_>>().join("、")), false)
            }
        }
        "web_search" => {
            let engine = input["engine"].as_str().unwrap_or(crate::locale::pick("google", "bing"));
            let ok = sys::shell_open(&search_url(&s("query"), engine));
            (if ok { format!("已在默认浏览器搜索：{}", s("query")) } else { "打开浏览器失败".into() }, !ok)
        }
        "open_url" => {
            let mut url = s("url");
            // obsidian:// 这类软件自己的链接协议原样打开（插件说明书会用到）；file:// 等不行
            if !app_link(&url) && !url.starts_with("http://") && !url.starts_with("https://") {
                url = format!("https://{url}");
            }
            let ok = sys::shell_open(&url);
            (if ok { format!("已打开：{url}") } else { "打开浏览器失败".into() }, !ok)
        }
        "current_time" => (sys::now_text(), false),
        "media_control" => {
            let key = match input["action"].as_str().unwrap_or("") {
                "volume_up" => MediaKey::VolumeUp,
                "volume_down" => MediaKey::VolumeDown,
                "mute" => MediaKey::Mute,
                "play_pause" => MediaKey::PlayPause,
                "next" => MediaKey::Next,
                "previous" => MediaKey::Previous,
                other => return Some((format!("未知操作：{other}"), true)),
            };
            let times = input["times"].as_u64().unwrap_or(if matches!(key, MediaKey::VolumeUp | MediaKey::VolumeDown) { 5 } else { 1 });
            sys::press_media_key(key, times.min(50) as u32);
            ("已完成".into(), false)
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_sites_encode_queries_and_preserve_aliases() {
        for (site, prefix) in [
            ("google", "https://www.google.com/search?q="),
            ("amazon", "https://www.amazon.com/s?k="),
            ("reddit", "https://www.reddit.com/search/?q="),
            ("x", "https://x.com/search?q="),
            ("wikipedia", "https://en.wikipedia.org/w/index.php?search="),
            ("spotify", "https://open.spotify.com/search/"),
        ] {
            assert_eq!(site_engine(site), Some(site));
            assert_eq!(search_url("a &b", site), format!("{prefix}a%20%26b"));
        }
        assert_eq!(site_engine("Twitter"), Some("x"));
        assert_eq!(site_engine("抖音"), Some("douyin"));
        assert_eq!(site_engine("B站"), Some("bilibili"));
        assert_eq!(site_engine("淘宝"), Some("taobao"));
        assert!(search_url("test", "").starts_with(crate::locale::pick("https://www.google.com/", "https://www.bing.com/")));
    }
}

/// App link schemes that open_url passes through unchanged (plugin guides use them):
/// any `scheme://` except dangerous ones, plus a few well-known schemes without `//`.
/// Anything else (e.g. "example.com" or "localhost:3000") is treated as a web address.
fn app_link(url: &str) -> bool {
    const BLOCKED: [&str; 5] = ["file", "ftp", "javascript", "data", "vbscript"];
    const NO_SLASH: [&str; 6] = ["mailto", "spotify", "tel", "callto", "sms", "msteams"];
    let Some((scheme, rest)) = url.split_once(':') else { return false };
    let scheme = scheme.to_ascii_lowercase();
    let valid = scheme.len() > 1 && scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '+' || c == '.');
    if !valid || BLOCKED.contains(&scheme.as_str()) || scheme == "http" || scheme == "https" {
        return false;
    }
    rest.starts_with("//") || NO_SLASH.contains(&scheme.as_str())
}

#[cfg(test)]
mod app_link_tests {
    use super::app_link;

    #[test]
    fn app_links() {
        assert!(app_link("obsidian://new?name=a"));
        assert!(app_link("spotify:search:lofi"));
        assert!(app_link("mailto:a@b.com?subject=Hi"));
        assert!(app_link("vscode://file/C:/x.txt"));
        assert!(!app_link("javascript:alert(1)"));
        assert!(!app_link("file:///C:/Windows"));
        assert!(!app_link("example.com"));
        assert!(!app_link("localhost:3000"));
        assert!(!app_link("https://example.com"));
    }
}
