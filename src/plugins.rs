//! 插件：教 Aeri 怎么用某个具体软件的“说明书”（Markdown），不是可执行代码。
//!
//! - 内置插件编译在程序里（仓库的 plugins/ 目录）；用户装的放在 %APPDATA%\VoiceAssistant\plugins\<id>\PLUGIN.md，同 id 时覆盖内置的。
//! - 前台是插件声明的程序、或者用户的话里提到插件的关键词时，自动把说明书带给大模型（with_screen 里）。
//! - 大模型照说明书用通用工具办事；为此提供 local_http（只能访问本机），说明书里的 {{secret:...}} 由本机读取后填入，不发给大模型。
//! - 插件市场：GitHub 上 yuqtai/aeri-plugins 仓库的 index.json，下载后按 SHA-256 校验。

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config;

const BUILTIN: &[&str] = &[include_str!("../plugins/siyuan/PLUGIN.md"), include_str!("../plugins/notepad/PLUGIN.md")];
pub const MARKET_REPO: &str = "yuqtai/aeri-plugins";
/// 一次最多自动带几份说明书，免得上下文太长
const MAX_AUTO: usize = 2;
const MAX_BODY: usize = 6000;

#[derive(Clone, Debug, PartialEq)]
pub struct Plugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    /// 程序文件名（小写），前台是它时自动启用
    pub matches: Vec<String>,
    /// 用户的话里出现这些词时自动启用（小写）
    pub keywords: Vec<String>,
    pub body: String,
    pub builtin: bool,
}

/// 解析 PLUGIN.md：开头 --- 之间是 key: value，后面是正文
pub fn parse(text: &str, builtin: bool) -> Option<Plugin> {
    let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    let (head, body) = (&rest[..end], rest[end + 4..].trim_start_matches('\n'));
    let mut get = std::collections::HashMap::new();
    for line in head.lines() {
        if let Some((k, v)) = line.split_once(':') {
            get.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    let list = |k: &str| -> Vec<String> {
        get.get(k).map(|v| v.split([',', '，']).map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect()).unwrap_or_default()
    };
    let id = get.get("id")?.clone();
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return None;
    }
    Some(Plugin {
        name: get.get("name").cloned().unwrap_or_else(|| id.clone()),
        version: get.get("version").cloned().unwrap_or_else(|| "0".into()),
        description: get.get("description").cloned().unwrap_or_default(),
        matches: list("match"),
        keywords: list("keywords"),
        body: body.trim().to_string(),
        builtin,
        id,
    })
}

pub fn dir() -> PathBuf {
    config::app_dir().join("plugins")
}

/// 用户装的插件
fn installed() -> Vec<Plugin> {
    std::fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path().join("PLUGIN.md")).ok())
        .filter_map(|t| parse(&t, false))
        .collect()
}

/// 全部可用插件：用户装的覆盖同 id 的内置插件，按名字排序
pub fn all() -> Vec<Plugin> {
    let mut list: Vec<Plugin> = BUILTIN.iter().filter_map(|t| parse(t, true)).collect();
    for p in installed() {
        list.retain(|b| b.id != p.id);
        list.push(p);
    }
    list.sort_by(|a, b| a.name.cmp(&b.name));
    list
}

/// 这句话和当前屏幕该带哪些说明书。exes：前台和后面几个窗口的程序名（前台在前）
pub fn matching(text: &str, exes: &[String]) -> Vec<Plugin> {
    let text = text.to_lowercase();
    let exes: Vec<String> = exes.iter().map(|e| e.to_lowercase()).collect();
    let mut hits: Vec<(usize, Plugin)> = all()
        .into_iter()
        .filter_map(|p| {
            // 话里提到的最优先，其次是前台程序，再次是后面的窗口
            let said = p.keywords.iter().any(|k| text.contains(k.as_str())) || text.contains(&p.name.to_lowercase());
            let pos = exes.iter().position(|e| p.matches.contains(e));
            match (said, pos) {
                (true, _) => Some((0, p)),
                (false, Some(0)) => Some((1, p)),
                _ => None,
            }
        })
        .collect();
    hits.sort_by_key(|(rank, _)| *rank);
    hits.into_iter().take(MAX_AUTO).map(|(_, p)| p).collect()
}

/// 自动带给大模型的说明
pub fn hint(text: &str, exes: &[String]) -> String {
    matching(text, exes)
        .iter()
        .map(|p| format!("【插件说明书：{} v{}，系统自动附上，照着做最快】\n{}", p.name, p.version, truncate(&p.body, MAX_BODY)))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { s.chars().take(n).collect::<String>() + "…" }
}

// ---------- 给大模型的工具 ----------

pub fn definitions() -> Vec<Value> {
    let names: Vec<String> = all().iter().map(|p| format!("{}（{}）", p.name, p.id)).collect();
    vec![
        json!({
            "name": "plugin_guide",
            "description": format!("读某个软件的插件说明书：怎么用最快的办法操作这个软件。已装的插件：{}。要操作这些软件、但系统没有自动附上说明书时先读它。", if names.is_empty() { "无".into() } else { names.join("、") }),
            "input_schema": {"type": "object", "properties": {"id": {"type": "string", "description": "插件 id 或名字"}}, "required": ["id"]}
        }),
        json!({
            "name": "local_http",
            "description": "调用这台电脑上软件提供的本机 HTTP 接口（只能访问 127.0.0.1 / localhost），按插件说明书用。说明书里的 {{secret:...}} 原样照抄，Aeri 会在本机读取后填进去。",
            "input_schema": {"type": "object", "properties": {
                "method": {"type": "string", "enum": ["GET", "POST", "PUT", "DELETE"]},
                "url": {"type": "string", "description": "http://127.0.0.1:端口/路径"},
                "headers": {"type": "object", "additionalProperties": {"type": "string"}},
                "body": {"description": "JSON 对象，或者字符串"}
            }, "required": ["url"]}
        }),
    ]
}

/// 插件市场工具只给前台（用户能确认）
pub fn market_definitions() -> Vec<Value> {
    vec![json!({
        "name": "plugin_market",
        "description": "插件市场：插件是教 Aeri 用某个软件的说明书。list 看有哪些（含已装的）；install 安装或更新（给 id）；remove 卸载（给 id）。用户问“能不能用某某软件”、要装插件时用。",
        "input_schema": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["list", "install", "remove"]},
            "id": {"type": "string"}
        }, "required": ["action"]}
    })]
}

pub async fn call(name: &str, input: &Value) -> Option<(String, bool)> {
    let out = match name {
        "plugin_guide" => guide(input["id"].as_str().unwrap_or("")),
        "local_http" => local_http(input).await,
        "plugin_market" => {
            let id = input["id"].as_str().unwrap_or("").trim();
            match input["action"].as_str().unwrap_or("list") {
                "install" => install(id).await.map(|p| format!("已安装插件“{}” v{}", p.name, p.version)),
                "remove" => remove(id).map(|n| format!("已卸载插件“{n}”")),
                _ => market_text().await,
            }
        }
        _ => return None,
    };
    Some(match out {
        Ok(s) => (s, false),
        Err(e) => (e, true),
    })
}

fn guide(id: &str) -> Result<String, String> {
    let key = id.trim().to_lowercase();
    all()
        .into_iter()
        .find(|p| p.id == key || p.name.to_lowercase() == key || p.keywords.contains(&key))
        .map(|p| format!("【{} v{}】\n{}", p.name, p.version, p.body))
        .ok_or_else(|| format!("没有“{id}”的插件，可以用 plugin_market 看看市场里有没有"))
}

// ---------- local_http ----------

fn local_only(url: &str) -> Result<(), String> {
    let u = reqwest::Url::parse(url).map_err(|_| "网址格式不对".to_string())?;
    let host = u.host_str().unwrap_or("");
    if u.scheme() != "http" && u.scheme() != "https" {
        return Err("只支持 http 网址".into());
    }
    if !matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1") {
        return Err("local_http 只能访问本机（127.0.0.1 / localhost）".into());
    }
    Ok(())
}

/// 把文本里的 {{secret:SPEC}} 换成本机读到的值，返回（替换后的文本，用到的密钥）
fn fill_secrets(text: &str, used: &mut Vec<String>) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("{{secret:") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 9..];
        let end = after.find("}}").ok_or("{{secret:...}} 没有结尾")?;
        let value = resolve_secret(&after[..end])?;
        used.push(value.clone());
        out.push_str(&value);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// SPEC：`文件#JSON指针`，可以用 | 接着往下读：上一步读到的值 + 这一段的路径后缀 = 下一个文件。
/// 例：`~/.config/siyuan/workspace.json#/0|/conf/conf.json#/api/token`
/// 只能读用户目录下 1 MB 以内的文件
pub fn resolve_secret(spec: &str) -> Result<String, String> {
    let mut prev = String::new();
    for seg in spec.split('|') {
        let (path, pointer) = seg.split_once('#').ok_or("密钥写法不对：缺少 #")?;
        let path = expand(&format!("{prev}{}", path.trim()));
        let home = std::env::var_os("USERPROFILE").map(PathBuf::from).ok_or("找不到用户目录")?;
        let canon = std::fs::canonicalize(&path).map_err(|_| format!("读不到 {}", path.display()))?;
        let home = std::fs::canonicalize(&home).unwrap_or(home);
        if !canon.starts_with(&home) {
            return Err("只能读用户目录下的文件".into());
        }
        if std::fs::metadata(&canon).map(|m| m.len()).unwrap_or(u64::MAX) > 1 << 20 {
            return Err("文件太大".into());
        }
        let v: Value = serde_json::from_slice(&std::fs::read(&canon).map_err(|e| e.to_string())?).map_err(|_| format!("{} 不是 JSON", path.display()))?;
        let found = v.pointer(pointer.trim()).ok_or_else(|| format!("{} 里没有 {pointer}", path.display()))?;
        prev = match found {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
    }
    if prev.is_empty() { Err("读到的值是空的".into()) } else { Ok(prev) }
}

/// ~ 换成用户目录，%VAR% 换成环境变量
fn expand(p: &str) -> PathBuf {
    let mut s = p.to_string();
    if let Some(rest) = s.strip_prefix('~') {
        s = format!("{}{rest}", std::env::var("USERPROFILE").unwrap_or_default());
    }
    while let Some(a) = s.find('%') {
        let Some(b) = s[a + 1..].find('%') else { break };
        let name = &s[a + 1..a + 1 + b];
        let val = std::env::var(name).unwrap_or_default();
        s = format!("{}{val}{}", &s[..a], &s[a + 2 + b..]);
    }
    PathBuf::from(s.replace('/', "\\"))
}

async fn local_http(input: &Value) -> Result<String, String> {
    let mut secrets = Vec::new();
    let url = fill_secrets(input["url"].as_str().unwrap_or(""), &mut secrets)?;
    local_only(&url)?;
    let method = input["method"].as_str().unwrap_or("POST").to_uppercase();
    let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| "method 不对")?;
    let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(15)).build().map_err(|e| e.to_string())?;
    let mut req = client.request(method, &url);
    if let Some(h) = input["headers"].as_object() {
        for (k, v) in h {
            req = req.header(k, fill_secrets(v.as_str().unwrap_or(""), &mut secrets)?);
        }
    }
    match &input["body"] {
        Value::Null => {}
        Value::String(s) => req = req.body(fill_secrets(s, &mut secrets)?),
        v => {
            let text = fill_secrets(&v.to_string(), &mut secrets)?;
            req = req.header("Content-Type", "application/json").body(text);
        }
    }
    let resp = req.send().await.map_err(|e| {
        if e.is_connect() { "连不上这个本机接口：对应的软件可能没打开".to_string() } else { format!("请求失败：{e}") }
    })?;
    let status = resp.status();
    let mut text = resp.text().await.unwrap_or_default();
    // 密钥不出现在返回给大模型的内容里
    for s in &secrets {
        if s.len() >= 4 {
            text = text.replace(s.as_str(), "***");
        }
    }
    let text = truncate(&text, 8000);
    if status.is_success() { Ok(format!("HTTP {status}\n{text}")) } else { Err(format!("HTTP {status}\n{text}")) }
}

// ---------- 市场 ----------

#[derive(Deserialize, Clone, Debug)]
pub struct Listing {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// 仓库里的相对路径，比如 siyuan/PLUGIN.md
    pub file: String,
    pub sha256: String,
}

#[derive(Deserialize)]
struct Index {
    plugins: Vec<Listing>,
}

/// 上次拿到的市场列表，界面刷新时用
static MARKET: Mutex<Vec<Listing>> = Mutex::new(Vec::new());

fn raw(path: &str) -> String {
    format!("https://raw.githubusercontent.com/{MARKET_REPO}/main/{path}")
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(format!("Aeri/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_default()
}

pub async fn fetch_market() -> Result<Vec<Listing>, String> {
    let resp = client().get(raw("index.json")).send().await.map_err(|_| "连不上插件市场（GitHub）".to_string())?;
    if !resp.status().is_success() {
        return Err(format!("插件市场返回 {}", resp.status()));
    }
    let idx: Index = resp.json().await.map_err(|_| "插件市场的列表看不懂".to_string())?;
    *MARKET.lock().unwrap() = idx.plugins.clone();
    Ok(idx.plugins)
}

pub fn cached_market() -> Vec<Listing> {
    MARKET.lock().unwrap().clone()
}

/// 版本号 a 比 b 新
pub fn newer(a: &str, b: &str) -> bool {
    let n = |s: &str| s.split('.').map(|x| x.trim().parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    n(a) > n(b)
}

async fn market_text() -> Result<String, String> {
    let market = fetch_market().await?;
    let have = all();
    let mut lines: Vec<String> = market
        .iter()
        .map(|l| {
            let state = match have.iter().find(|p| p.id == l.id) {
                Some(p) if newer(&l.version, &p.version) => format!("已装 v{}，可更新到 v{}", p.version, l.version),
                Some(p) if p.builtin => "内置".into(),
                Some(_) => "已安装".into(),
                None => "未安装".into(),
            };
            format!("{}（id: {}）：{}——{state}", l.name, l.id, l.description)
        })
        .collect();
    for p in have.iter().filter(|p| !market.iter().any(|l| l.id == p.id)) {
        lines.push(format!("{}（id: {}）：{}——{}", p.name, p.id, p.description, if p.builtin { "内置" } else { "已安装（本地）" }));
    }
    Ok(lines.join("\n"))
}

pub async fn install(id: &str) -> Result<Plugin, String> {
    let market = match cached_market() {
        m if m.iter().any(|l| l.id == id) => m,
        _ => fetch_market().await?,
    };
    let l = market.iter().find(|l| l.id == id).ok_or_else(|| format!("插件市场里没有“{id}”"))?;
    let resp = client().get(raw(&l.file)).send().await.map_err(|_| "下载插件失败".to_string())?;
    if !resp.status().is_success() {
        return Err(format!("下载插件失败：{}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|_| "下载插件失败".to_string())?;
    let hash: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if !hash.eq_ignore_ascii_case(l.sha256.trim()) {
        return Err("插件校验不通过（文件和市场登记的不一致），没有安装".into());
    }
    let text = String::from_utf8(bytes.to_vec()).map_err(|_| "插件不是 UTF-8 文本".to_string())?;
    let p = parse(&text, false).ok_or("插件格式不对")?;
    if p.id != l.id {
        return Err("插件 id 和市场登记的不一致，没有安装".into());
    }
    let d = dir().join(&p.id);
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    std::fs::write(d.join("PLUGIN.md"), &text).map_err(|e| e.to_string())?;
    Ok(p)
}

/// 卸载用户装的插件；内置插件卸载不了（装过更新版的话，卸载后回到内置版本）
pub fn remove(id: &str) -> Result<String, String> {
    let d = dir().join(id);
    let name = installed().into_iter().find(|p| p.id == id).map(|p| p.name);
    match name {
        Some(n) => {
            std::fs::remove_dir_all(&d).map_err(|e| e.to_string())?;
            Ok(n)
        }
        None if all().iter().any(|p| p.id == id && p.builtin) => Err("这是内置插件，不能卸载".into()),
        None => Err(format!("没有安装“{id}”")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_parse() {
        let list: Vec<Plugin> = BUILTIN.iter().filter_map(|t| parse(t, true)).collect();
        assert_eq!(list.len(), BUILTIN.len());
        let s = list.iter().find(|p| p.id == "siyuan").unwrap();
        assert_eq!(s.matches, vec!["siyuan.exe"]);
        assert!(s.keywords.contains(&"思源".to_string()));
        assert!(s.body.starts_with("思源是"));
    }

    #[test]
    fn matching_by_words_and_front_window() {
        let ids = |t: &str, e: &[&str]| -> Vec<String> {
            matching(t, &e.iter().map(|s| s.to_string()).collect::<Vec<_>>()).into_iter().map(|p| p.id).collect()
        };
        assert_eq!(ids("记到思源笔记里吧", &["msedge.exe"]), vec!["siyuan"]);
        assert_eq!(ids("把这段保存一下", &["SiYuan.exe"]), vec!["siyuan"]);
        // 只在后面的窗口里不算
        assert!(ids("今天天气怎么样", &["msedge.exe", "SiYuan.exe"]).is_empty());
    }

    #[test]
    fn only_local_urls() {
        assert!(local_only("http://127.0.0.1:6806/api/x").is_ok());
        assert!(local_only("http://localhost:27123/").is_ok());
        assert!(local_only("https://example.com/").is_err());
        assert!(local_only("http://127.0.0.1.evil.com/").is_err());
        assert!(local_only("file:///C:/x").is_err());
    }

    #[test]
    fn secrets_from_chained_json() {
        let home = std::env::var("USERPROFILE").unwrap();
        let d = PathBuf::from(&home).join(".aeri-plugin-test");
        std::fs::create_dir_all(d.join("ws").join("conf")).unwrap();
        let ws = d.join("ws").to_string_lossy().replace('\\', "/");
        std::fs::write(d.join("workspace.json"), serde_json::to_string(&json!([ws])).unwrap()).unwrap();
        std::fs::write(d.join("ws").join("conf").join("conf.json"), r#"{"api":{"token":"abc123"}}"#).unwrap();
        let spec = "~/.aeri-plugin-test/workspace.json#/0|/conf/conf.json#/api/token";
        assert_eq!(resolve_secret(spec).unwrap(), "abc123");
        let mut used = Vec::new();
        assert_eq!(fill_secrets(&format!("Token {{{{secret:{spec}}}}}"), &mut used).unwrap(), "Token abc123");
        assert_eq!(used, vec!["abc123"]);
        assert!(resolve_secret("C:/Windows/win.ini#/x").is_err(), "用户目录外的文件不能读");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn version_compare() {
        assert!(newer("1.0.1", "1.0.0"));
        assert!(newer("1.10.0", "1.9.9"));
        assert!(!newer("1.0.0", "1.0.0"));
    }
}
