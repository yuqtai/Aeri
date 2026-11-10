//! 历史记录：对话和后台任务按天追加到 %APPDATA%\VoiceAssistant\history\YYYY-MM-DD.jsonl，保留 30 天。
//! 用途：重启后把最近的对话显示回来；设置里“查看历史记录”生成一个可搜索的网页；
//! 助手可以用 history_search 回答“我昨天让你做了什么”。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::assistant::{Role, UiEvent};
use crate::config;

const KEEP_DAYS: i64 = 30;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    /// HH:MM:SS
    pub t: String,
    /// "chat" 或 "task"
    pub kind: String,
    /// chat：0 用户 1 助手 2 工具 3 系统
    #[serde(default)]
    pub role: i32,
    #[serde(default)]
    pub text: String,
    /// task：编号、执行者、状态（done / failed / cancelled）、耗时秒数、完整结果
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskRecord>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TaskRecord {
    pub id: u32,
    pub executor: String,
    pub state: String,
    pub secs: u64,
    pub description: String,
    pub result: String,
}

static ENABLED: Mutex<bool> = Mutex::new(true);
static WRITE: Mutex<()> = Mutex::new(());

pub fn dir() -> PathBuf {
    config::app_dir().join("history")
}

pub fn set_enabled(on: bool) {
    *ENABLED.lock().unwrap() = on;
}

fn enabled() -> bool {
    *ENABLED.lock().unwrap()
}

/// (YYYY-MM-DD, HH:MM:SS)
fn now() -> (String, String) {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    (format!("{:04}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay), format!("{:02}:{:02}:{:02}", t.wHour, t.wMinute, t.wSecond))
}

fn append(e: &Entry, day: &str) {
    // 单元测试不写用户的真实记录
    if !enabled() || cfg!(test) {
        return;
    }
    let _g = WRITE.lock().unwrap();
    let _ = std::fs::create_dir_all(dir());
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(dir().join(format!("{day}.jsonl"))) {
        if let Ok(line) = serde_json::to_string(e) {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// 界面事件抄一份：只记对话，进展条太碎不记（任务结束时另记完整结果）
pub fn observe(ev: &UiEvent) {
    if let UiEvent::Chat(role, text) = ev {
        record_chat(*role, text);
    }
}

pub fn record_chat(role: Role, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    let (day, t) = now();
    append(&Entry { t, kind: "chat".into(), role: role as i32, text: text.to_string(), task: None }, &day);
}

pub fn record_task(rec: TaskRecord) {
    let (day, t) = now();
    let text = format!("任务 #{} {}", rec.id, state_name(&rec.state));
    append(&Entry { t, kind: "task".into(), role: Role::System as i32, text, task: Some(rec) }, &day);
}

fn state_name(s: &str) -> &'static str {
    match s {
        "done" => "已完成",
        "failed" => "失败",
        "cancelled" => "已取消",
        _ => "进行中",
    }
}

/// 有记录的日期，新的在前
pub fn days() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".jsonl")).map(String::from))
        .filter(|d| d.len() == 10)
        .collect();
    v.sort_unstable_by(|a, b| b.cmp(a));
    v
}

pub fn read_day(day: &str) -> Vec<Entry> {
    std::fs::read_to_string(dir().join(format!("{day}.jsonl")))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// 最近的 n 条对话（跨天），用于启动时显示回界面；返回 (日期, 条目)
pub fn recent_chat(n: usize) -> Vec<(String, Entry)> {
    let mut out = Vec::new();
    for day in days() {
        let mut items: Vec<(String, Entry)> = read_day(&day).into_iter().filter(|e| e.kind == "chat").map(|e| (day.clone(), e)).collect();
        items.reverse();
        out.extend(items);
        if out.len() >= n {
            break;
        }
    }
    out.truncate(n);
    out.reverse();
    out
}

/// 删掉 30 天前的记录
pub fn prune() {
    let days = days();
    let Some(today) = days.first() else { return };
    let cutoff = day_number(today) - KEEP_DAYS;
    for d in days.iter().filter(|d| day_number(d) < cutoff) {
        let _ = std::fs::remove_file(dir().join(format!("{d}.jsonl")));
    }
}

/// 日期换成连续的天数，只用来比较先后
fn day_number(d: &str) -> i64 {
    let p: Vec<i64> = d.split('-').filter_map(|x| x.parse().ok()).collect();
    if p.len() != 3 {
        return 0;
    }
    let (y, m, day) = (p[0], p[1], p[2]);
    let (y, m) = if m <= 2 { (y - 1, m + 12) } else { (y, m) };
    365 * y + y / 4 - y / 100 + y / 400 + (153 * (m - 3) + 2) / 5 + day
}

pub fn clear() -> std::io::Result<()> {
    let _g = WRITE.lock().unwrap();
    match std::fs::remove_dir_all(dir()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

// ---------- 网页查看 ----------

/// 生成 history.html 并用默认浏览器打开
pub fn open_viewer() -> Result<(), String> {
    let mut data = Vec::new();
    for day in days() {
        data.push(json!({"day": day, "items": read_day(&day)}));
    }
    let html = VIEWER.replace("__DATA__", &serde_json::to_string(&data).unwrap_or_else(|_| "[]".into()).replace("</", "<\\/"));
    let path = config::app_dir().join("history.html");
    std::fs::write(&path, html).map_err(|e| format!("写入失败：{e}"))?;
    if crate::sys::shell_open(&path.to_string_lossy()) {
        Ok(())
    } else {
        Err("打不开浏览器".into())
    }
}

/// 历史记录网页（单文件，数据替换 __DATA__ 后写到 history.html）
const VIEWER: &str = include_str!("history_viewer.html");

// ---------- 给助手用的工具 ----------

pub fn definitions() -> Vec<Value> {
    vec![json!({
        "name": "history_search",
        "description": "Search previous conversations and background task records, retained for 30 days. Use for what the user asked yesterday, last week's task results or something said earlier. date is YYYY-MM-DD (use current_time first to check today); query is a keyword. Omit both for recent records.",
        "input_schema": {"type": "object", "properties": {
            "date": {"type": "string", "description": "YYYY-MM-DD; restrict to this day."},
            "query": {"type": "string", "description": "Keyword matching conversation content, task requests and results."},
            "tasks_only": {"type": "boolean", "description": "Only background tasks."}
        }}
    })]
}

pub fn call(name: &str, input: &Value) -> Option<(String, bool)> {
    (name == "history_search").then(|| (search(input), false))
}

fn search(input: &Value) -> String {
    let date = input["date"].as_str().map(str::trim).filter(|s| !s.is_empty());
    let query = input["query"].as_str().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
    let tasks_only = input["tasks_only"].as_bool().unwrap_or(false);
    let days: Vec<String> = match date {
        Some(d) => vec![d.to_string()],
        None => days(),
    };
    let mut lines = Vec::new();
    'outer: for day in days {
        let mut found = Vec::new();
        for e in read_day(&day) {
            if tasks_only && e.kind != "task" {
                continue;
            }
            let line = match &e.task {
                Some(t) => format!("{} 任务#{}（{}，{}）要求：{} 结果：{}", e.t, t.id, t.executor, state_name(&t.state), t.description, t.result.chars().take(300).collect::<String>()),
                None => format!("{} {}：{}", e.t, ["用户", "助手", "工具", "系统"].get(e.role as usize).unwrap_or(&"助手"), e.text.chars().take(200).collect::<String>()),
            };
            if query.as_ref().is_some_and(|q| !line.to_lowercase().contains(q)) {
                continue;
            }
            found.push(line);
        }
        if !found.is_empty() {
            lines.push(format!("【{day}】"));
            // 一天太多时只留最后 40 条
            let skip = found.len().saturating_sub(40);
            lines.extend(found.into_iter().skip(skip));
        }
        if lines.len() > 120 || date.is_none() && query.is_none() && lines.len() > 40 {
            break 'outer;
        }
    }
    if lines.is_empty() {
        "没有找到相关记录（只保留最近 30 天）".into()
    } else {
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_order() {
        assert_eq!(day_number("2026-03-01") - day_number("2026-02-28"), 1);
        assert_eq!(day_number("2026-01-01") - day_number("2025-12-31"), 1);
        assert_eq!(day_number("2026-11-08") - day_number("2026-10-09"), 30);
    }

    #[test]
    fn entry_roundtrip() {
        let e = Entry { t: "12:00:00".into(), kind: "chat".into(), role: 0, text: "你好".into(), task: None };
        let s = serde_json::to_string(&e).unwrap();
        assert!(!s.contains("task"));
        assert_eq!(serde_json::from_str::<Entry>(&s).unwrap(), e);
    }
}
