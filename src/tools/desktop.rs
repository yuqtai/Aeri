//! 桌面控制：调用随程序附带的 agent-desktop（https://github.com/vercel-labs/agent-desktop）。
//! 它通过 Windows UI Automation 读控件树、按编号操作控件；点击、设值、勾选这些默认走无障碍接口，
//! 不移动鼠标、不抢焦点，操作后还会读回状态确认是否生效。
//! 每个命令是一次独立的进程调用（启动约 30 毫秒），结果是一段 JSON；这里把它整理成简短的中文文字交给模型，
//! 快照里的控件树也压成一行一个控件，免得原始 JSON（Edge 一个窗口就有几十 KB）把模型的上下文撑满。

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::process::Command;

use crate::config::NoWindow;
use crate::screen;

const EXE: &str = "agent-desktop.exe";
const TIMEOUT: Duration = Duration::from_secs(30);
/// 快照最多列多少行；控件再多模型也看不过来
const MAX_LINES: usize = 200;
const MAX_NAME: usize = 60;
/// 网页里的控件层级很深，默认的 10 层读不到正文
const MAX_DEPTH: &str = "30";

pub struct Desktop {
    exe: Option<PathBuf>,
    /// 最近一次快照的编号，控件编号（e12）要带上它才能定位
    snapshot: Mutex<Option<String>>,
}

impl Desktop {
    pub fn new() -> Self {
        Self { exe: locate(), snapshot: Mutex::new(None) }
    }

    /// 执行工具，返回（结果文字，是否出错）
    pub async fn call(&self, name: &str, input: &Value) -> (String, bool) {
        match self.handle(name, input).await {
            Ok(text) => (text, false),
            Err(text) => (text, true),
        }
    }

    async fn handle(&self, name: &str, input: &Value) -> Result<String, String> {
        let s = |k: &str| input[k].as_str().unwrap_or("").trim().to_string();
        match name {
            "desktop_windows" => {
                let wins = self.run(&["list-windows"]).await?;
                let own = own_exe();
                let lines: Vec<String> = wins
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|w| !w["app_name"].as_str().is_some_and(|a| a.eq_ignore_ascii_case(&own)))
                    .map(|w| {
                        let min = if w["minimized"].as_bool() == Some(true) { "，最小化" } else { "" };
                        format!("{}（{}{min}）", clean_title(w["title"].as_str().unwrap_or("")), w["app_name"].as_str().unwrap_or(""))
                    })
                    .collect();
                Ok(format!("已打开的窗口：\n{}", lines.join("\n")))
            }
            "desktop_focus" => {
                let (id, title) = self.find_window(&s("name")).await?;
                self.run(&["restore", "--window-id", &id]).await.ok(); // 最小化的窗口要先还原，没最小化时这一步无害
                self.run(&["focus-window", "--window-id", &id]).await?;
                Ok(format!("已切换到：{title}"))
            }
            "desktop_snapshot" => self.snapshot(&s("window")).await,
            "desktop_click" => {
                let r = self.qualify(input)?;
                if input["double"].as_bool() == Some(true) {
                    self.run(&["--headed", "double-click", &r]).await?;
                    Ok(format!("已双击 {}", short(&r)))
                } else {
                    let data = self.run(&["click", &r]).await?;
                    Ok(format!("已点击 {}{}", short(&r), verified(&data)))
                }
            }
            "desktop_type" => {
                let r = self.qualify(input)?;
                let data = self.run(&["--headed", "type", &r, input["text"].as_str().unwrap_or("")]).await?;
                Ok(format!("已输入{}", verified(&data)))
            }
            "desktop_set_value" => {
                let r = self.qualify(input)?;
                let data = self.run(&["set-value", &r, input["value"].as_str().unwrap_or("")]).await?;
                Ok(format!("已填入 {}{}", short(&r), verified(&data)))
            }
            "desktop_select" => {
                let r = self.qualify(input)?;
                // 有的程序（比如 WinForms）的下拉选项在单独的弹出窗口里，直接选会一直等到超时
                let data = self.run(&["select", &r, &s("option"), "--timeout-ms", "3000"]).await.map_err(|e| {
                    format!("{e}\n可以改用 desktop_click 点开下拉框，再 desktop_snapshot 找到选项点击。")
                })?;
                Ok(format!("已选择“{}”{}", s("option"), verified(&data)))
            }
            "desktop_check" => {
                let r = self.qualify(input)?;
                let (cmd, done) = match input["checked"].as_bool() {
                    Some(true) => ("check", "已勾选"),
                    Some(false) => ("uncheck", "已取消勾选"),
                    None => ("toggle", "已切换"),
                };
                let data = self.run(&[cmd, &r]).await?;
                Ok(format!("{done} {}{}", short(&r), verified(&data)))
            }
            "desktop_scroll" => {
                let r = self.qualify(input)?;
                let direction = match s("direction").as_str() {
                    d @ ("up" | "left" | "right") => d.to_string(),
                    _ => "down".to_string(),
                };
                let amount = input["amount"].as_u64().unwrap_or(3).clamp(1, 20).to_string();
                self.run(&["scroll", &r, "--direction", &direction, "--amount", &amount]).await?;
                Ok("已滚动，内容变了，需要的话重新 desktop_snapshot".into())
            }
            "desktop_keys" => {
                let keys = normalize_keys(&s("keys"));
                if keys.is_empty() {
                    return Err("keys 为空".into());
                }
                self.run(&["press", &keys]).await?;
                Ok(format!("已按下 {}", s("keys")))
            }
            "clipboard_read" => {
                let data = self.run(&["clipboard-get"]).await?;
                let text = data["text"].as_str().or(data.as_str()).unwrap_or("");
                if text.trim().is_empty() {
                    return Ok("剪贴板里没有文字".into());
                }
                Ok(format!("剪贴板里的文字：\n{}", clip(text, 6000)))
            }
            "clipboard_write" => {
                let text = input["text"].as_str().unwrap_or("");
                if text.trim().is_empty() {
                    return Err("text 为空".into());
                }
                self.run(&["clipboard-set", text]).await?;
                Ok(format!("已复制到剪贴板（{} 个字）", text.chars().count()))
            }
            other => Err(format!("未知工具：{other}")),
        }
    }

    /// 运行一条 agent-desktop 命令，成功时返回 data，失败时返回给模型看的错误说明
    async fn run(&self, args: &[&str]) -> Result<Value, String> {
        let exe = self.exe.as_ref().ok_or_else(|| format!("没有找到桌面控制组件 {EXE}，它应该放在 voa.exe 旁边，请重新安装"))?;
        let mut cmd = Command::new(exe);
        cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
        cmd.creation_flags_no_window();
        let out = tokio::time::timeout(TIMEOUT, cmd.output())
            .await
            .map_err(|_| "桌面控制超时（30 秒没有响应）".to_string())?
            .map_err(|e| format!("启动桌面控制组件失败：{e}"))?;
        let reply: Value = serde_json::from_slice(&out.stdout).map_err(|_| "桌面控制组件没有返回结果".to_string())?;
        if reply["ok"].as_bool() == Some(true) {
            return Ok(reply["data"].clone());
        }
        Err(describe_error(&reply["error"]))
    }

    /// 按标题找窗口，返回（窗口 id，标题）
    async fn find_window(&self, name: &str) -> Result<(String, String), String> {
        if name.is_empty() {
            return Err("需要窗口标题".into());
        }
        let needle = name.to_lowercase();
        let own = own_exe();
        let wins = self.run(&["list-windows"]).await?;
        wins.as_array()
            .into_iter()
            .flatten()
            .filter(|w| !w["app_name"].as_str().is_some_and(|a| a.eq_ignore_ascii_case(&own)))
            .find(|w| {
                w["title"].as_str().is_some_and(|t| clean_title(t).to_lowercase().contains(&needle))
                    || w["app_name"].as_str().is_some_and(|a| a.to_lowercase().trim_end_matches(".exe") == needle)
            })
            .map(|w| (w["id"].as_str().unwrap_or("").to_string(), clean_title(w["title"].as_str().unwrap_or(""))))
            .ok_or_else(|| format!("没有找到标题包含“{name}”的窗口，可以先用 desktop_windows 查看"))
    }

    async fn snapshot(&self, window: &str) -> Result<String, String> {
        let id = if window.is_empty() {
            // 用户正在看的窗口（跳过助手自己的）；agent-desktop 的窗口 id 就是 w-窗口句柄
            let (hwnd, _) = screen::front_window().ok_or("没有找到用户正在看的窗口")?;
            format!("w-{}", hwnd.0 as usize)
        } else {
            self.find_window(window).await?.0
        };
        let data = self.run(&["snapshot", "--window-id", &id, "--max-depth", MAX_DEPTH, "--compact"]).await?;
        let snap = data["snapshot_id"].as_str().unwrap_or("").to_string();
        *self.snapshot.lock().unwrap() = Some(snap);
        let title = clean_title(data["window"]["title"].as_str().unwrap_or(""));
        let mut lines = Vec::new();
        render(&data["tree"], &mut lines);
        let total = lines.len();
        lines.truncate(MAX_LINES);
        if total > MAX_LINES {
            lines.push(format!("……（还有 {} 行，已截断）", total - MAX_LINES));
        }
        Ok(format!("窗口：{title}\n控件编号（操作时填 ref，比如 e3）：\n{}", lines.join("\n")))
    }

    /// 模型给的编号（e12、12 或完整的 @快照:e12）补全成 agent-desktop 要的完整编号
    fn qualify(&self, input: &Value) -> Result<String, String> {
        let raw = match &input["ref"] {
            Value::String(s) => s.trim().to_string(),
            Value::Number(n) => n.to_string(),
            _ => match &input["id"] {
                Value::Number(n) => n.to_string(),
                _ => return Err("需要控件编号 ref（先用 desktop_snapshot 拿到）".into()),
            },
        };
        if raw.starts_with('@') && raw.contains(':') {
            return Ok(raw);
        }
        let n = raw.trim_start_matches('@').trim_start_matches(['e', 'E']);
        if n.is_empty() || !n.chars().all(|c| c.is_ascii_digit()) {
            return Err(format!("控件编号“{raw}”不对，应该是 desktop_snapshot 里的 e 加数字，比如 e3"));
        }
        let snap = self.snapshot.lock().unwrap().clone().ok_or("还没有读取过窗口，请先调用 desktop_snapshot")?;
        Ok(format!("@{snap}:e{n}"))
    }
}

/// agent-desktop.exe 放在 voa.exe 旁边；开发时也可以装在 PATH 里
fn locate() -> Option<PathBuf> {
    let beside = std::env::current_exe().ok()?.with_file_name(EXE);
    if beside.exists() {
        return Some(beside);
    }
    std::env::var_os("PATH")?.to_str()?.split(';').map(|d| PathBuf::from(d).join(EXE)).find(|p| p.exists())
}

fn own_exe() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "voa.exe".into())
}

/// Edge 的标题里夹着零宽空格（Microsoft\u{200b} Edge）
fn clean_title(t: &str) -> String {
    t.replace('\u{200b}', "").trim().to_string()
}

fn short(r: &str) -> &str {
    r.rsplit(':').next().unwrap_or(r)
}

/// 读回状态没对上时提醒模型重新看一下，不要当成已经成功
fn verified(data: &Value) -> &'static str {
    match data["disposition"]["delivery"].as_str() {
        Some("delivered_verified") => "（已确认生效）",
        Some("delivered_unverified") => "（程序没有反馈结果，需要的话重新 desktop_snapshot 确认）",
        _ => "",
    }
}

/// 常见错误换成模型容易理解的说法，其余的保留原始信息
fn describe_error(e: &Value) -> String {
    let code = e["code"].as_str().unwrap_or("");
    let message = e["message"].as_str().unwrap_or("未知错误");
    match code {
        "STALE_REF" | "SNAPSHOT_NOT_FOUND" | "AMBIGUOUS_TARGET" | "ELEMENT_NOT_FOUND" => {
            format!("控件编号已失效（界面变了），请重新 desktop_snapshot 再操作。（{code}：{message}）")
        }
        "POLICY_DENIED" => format!("这个操作被安全策略拒绝了：{message}"),
        "WINDOW_NOT_FOUND" | "APP_NOT_FOUND" => format!("没有找到窗口：{message}，可以先用 desktop_windows 查看"),
        "ACTION_FAILED" if e["details"]["kind"].as_str() == Some("post_action_verification") => {
            format!("操作已执行，但读回来的状态和预期不一样，不要重复操作，先 desktop_snapshot 看看现在的样子。（{message}）")
        }
        _ => format!("操作失败（{code}）：{message}"),
    }
}

/// 控件树压成一行一个：可操作的控件带编号，纯文字只列出来当上下文。
/// 没有名称也没有值的控件（模型认不出是什么）和禁用的控件（点了也没用）不列，Edge 工具栏里这类占了一大半
fn render(node: &Value, lines: &mut Vec<String>) {
    let role = node["role"].as_str().unwrap_or("");
    let name = node["name"].as_str().map(str::trim).unwrap_or("");
    let value = node["value"].as_str().map(str::trim).filter(|v| !v.is_empty() && *v != name);
    let states: Vec<&str> = node["states"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let usable = (!name.is_empty() || value.is_some()) && !states.contains(&"disabled");
    if let Some(r) = node["ref_id"].as_str().filter(|_| usable) {
        let mut line = format!("[{}] {role}", short(r));
        if !name.is_empty() {
            line.push_str(&format!(" \"{}\"", clip_line(name)));
        }
        if let Some(v) = value {
            line.push_str(&format!(" = \"{}\"", clip_line(v)));
        }
        let states: Vec<&str> =
            states.into_iter().filter(|s| matches!(*s, "checked" | "selected" | "expanded" | "collapsed" | "focused")).collect();
        if !states.is_empty() {
            line.push_str(&format!(" ({})", states.join(",")));
        }
        lines.push(line);
    } else if !name.is_empty() && matches!(role, "statictext" | "text" | "heading" | "label") {
        lines.push(format!("  \"{}\"", clip_line(name)));
    }
    for child in node["children"].as_array().into_iter().flatten() {
        render(child, lines);
    }
}

fn clip_line(s: &str) -> String {
    let s = s.replace(['\n', '\r'], " ");
    if s.chars().count() <= MAX_NAME {
        return s;
    }
    format!("{}…", s.chars().take(MAX_NAME).collect::<String>())
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}……（后面还有，已截断）")
}

/// 模型习惯的写法换成 agent-desktop 的键名："Enter" -> "return"，"win+d" -> "meta+d"，"{ctrl}(s)" -> "ctrl+s"
fn normalize_keys(keys: &str) -> String {
    let keys = keys.replace(['{', '('], "+").replace(['}', ')'], "").replace(' ', "");
    keys.split('+')
        .filter(|k| !k.is_empty())
        .map(|k| {
            let lower = k.to_lowercase();
            match lower.as_str() {
                "enter" => "return".to_string(),
                "esc" => "escape".to_string(),
                "del" => "delete".to_string(),
                "win" | "windows" => "meta".to_string(),
                "control" => "ctrl".to_string(),
                "pgup" => "pageup".to_string(),
                "pgdn" => "pagedown".to_string(),
                _ => lower, // 单个大写字母会被当成 shift+字母，统一用小写
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

pub fn definitions() -> Vec<Value> {
    let by_ref = |desc: &str, extra: Value, required: &[&str]| {
        let mut props = json!({"ref": {"type": "string", "description": "desktop_snapshot 里的控件编号，比如 e3"}});
        props.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
        let mut req = vec!["ref"];
        req.extend_from_slice(required);
        (desc.to_string(), json!({"type": "object", "properties": props, "required": req}))
    };
    let mut defs = vec![
        json!({
            "name": "desktop_windows",
            "description": "列出当前打开的窗口（标题和程序名）。",
            "input_schema": {"type": "object", "properties": {}}
        }),
        json!({
            "name": "desktop_focus",
            "description": "把标题包含 name 的窗口切到前台（最小化的会先还原）。也可以填程序名，比如 notepad。注意窗口标题可能是英文（如记事本叫 Notepad）。",
            "input_schema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}
        }),
        json!({
            "name": "desktop_snapshot",
            "description": "读取窗口里的控件列表：每行是编号、类型、名称、当前值和状态。不填 window 时读用户正在看的窗口；窗口在后面也能读，不用先切到前台。操作控件前先调用它拿编号，界面变化后要重新读。",
            "input_schema": {"type": "object", "properties": {"window": {"type": "string", "description": "窗口标题的一部分，可选"}}}
        }),
    ];
    for (name, desc, extra, required) in [
        ("desktop_click", "点击控件（按钮、链接、菜单项、标签页等）。默认不移动鼠标、不抢焦点；double 为 true 时会真的移动鼠标双击。", json!({"double": {"type": "boolean", "description": "是否双击"}}), &[][..]),
        ("desktop_type", "在输入框里模拟键盘输入文字（支持中文），会把窗口切到前台。要整体替换输入框内容优先用 desktop_set_value。", json!({"text": {"type": "string"}}), &["text"][..]),
        ("desktop_set_value", "直接把输入框的内容设为 value（替换原有内容），不需要切到前台，比模拟键盘可靠。", json!({"value": {"type": "string"}}), &["value"][..]),
        ("desktop_select", "在下拉框或列表里选中名为 option 的一项。", json!({"option": {"type": "string"}}), &["option"][..]),
        ("desktop_check", "勾选或取消勾选复选框、开关：checked 为 true 勾选，false 取消，不填就切换。", json!({"checked": {"type": "boolean"}}), &[][..]),
        ("desktop_scroll", "滚动控件（列表、文档、网页区域），direction 为 up/down/left/right，amount 为滚动格数（默认 3）。", json!({"direction": {"type": "string", "enum": ["up", "down", "left", "right"]}, "amount": {"type": "integer"}}), &[][..]),
    ] {
        let (desc, schema) = by_ref(desc, extra, required);
        defs.push(json!({"name": name, "description": desc, "input_schema": schema}));
    }
    defs.extend([
        json!({
            "name": "desktop_keys",
            "description": "向前台窗口按快捷键或特殊键，例如 ctrl+s、ctrl+shift+t、enter、esc、tab、f5。关闭窗口、锁屏、切换窗口这类系统快捷键会被拒绝，关闭程序请用 close_app。",
            "input_schema": {"type": "object", "properties": {"keys": {"type": "string"}}, "required": ["keys"]}
        }),
        json!({
            "name": "clipboard_write",
            "description": "把文字直接放进剪贴板，用户随后就能粘贴。复制屏幕上的某部分内容时，先 screen_read 拿到文字，挑出用户要的那部分写进来，比模拟全选、复制准确。",
            "input_schema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}
        }),
        json!({
            "name": "clipboard_read",
            "description": "读取剪贴板里的文字，比如用户说“我刚复制的这段”。",
            "input_schema": {"type": "object", "properties": {}}
        }),
    ]);
    defs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_keys() {
        assert_eq!(normalize_keys("ctrl+s"), "ctrl+s");
        assert_eq!(normalize_keys("Ctrl+Shift+T"), "ctrl+shift+t");
        assert_eq!(normalize_keys("Enter"), "return");
        assert_eq!(normalize_keys("esc"), "escape");
        assert_eq!(normalize_keys("win+d"), "meta+d");
        assert_eq!(normalize_keys("{ctrl}(a)"), "ctrl+a");
    }

    #[test]
    fn qualifies_refs() {
        let d = Desktop { exe: None, snapshot: Mutex::new(Some("s1abc".into())) };
        assert_eq!(d.qualify(&json!({"ref": "e3"})).unwrap(), "@s1abc:e3");
        assert_eq!(d.qualify(&json!({"ref": "@e3"})).unwrap(), "@s1abc:e3");
        assert_eq!(d.qualify(&json!({"id": 7})).unwrap(), "@s1abc:e7");
        assert_eq!(d.qualify(&json!({"ref": "@s9:e2"})).unwrap(), "@s9:e2");
        assert!(d.qualify(&json!({"ref": "保存"})).is_err());
        assert!(d.qualify(&json!({})).is_err());
    }

    #[test]
    fn renders_tree() {
        let tree = json!({"role": "window", "name": "无标题 - Notepad", "children": [
            {"role": "button", "name": "保存", "ref_id": "@s1:e1"},
            {"role": "statictext", "name": "第 1 行"},
            {"role": "menubutton", "ref_id": "@s1:e4"},
            {"role": "button", "name": "翻译", "ref_id": "@s1:e5", "states": ["disabled"]},
            {"role": "group", "children": [
                {"role": "textfield", "name": "地址栏", "value": "https://example.com", "ref_id": "@s1:e2", "states": ["focused", "visible"]},
                {"role": "checkbox", "name": "自动换行", "ref_id": "@s1:e3", "states": ["checked"]}
            ]}
        ]});
        let mut lines = Vec::new();
        render(&tree, &mut lines);
        assert_eq!(lines, vec![
            "[e1] button \"保存\"",
            "  \"第 1 行\"",
            "[e2] textfield \"地址栏\" = \"https://example.com\" (focused)",
            "[e3] checkbox \"自动换行\" (checked)",
        ]);
    }

    /// 手动测试：`cargo test snapshots_front_window -- --ignored --nocapture`（agent-desktop.exe 要在 PATH 里或测试程序旁边）
    #[tokio::test]
    #[ignore]
    async fn snapshots_front_window() {
        let d = Desktop::new();
        let t = std::time::Instant::now();
        let (out, err) = d.call("desktop_snapshot", &json!({})).await;
        println!("耗时 {:?}，出错 {err}", t.elapsed());
        println!("{}", out.chars().take(2000).collect::<String>());
    }
}
