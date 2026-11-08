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

const VIEWER: &str = r#"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Aeri 历史记录</title><style>
:root{--bg:#f6f7fb;--card:#fff;--text:#111827;--sub:#6b7280;--line:#e5e7eb;--accent:#4f46e5;--soft:#eef2ff;--ok:#16a34a;--bad:#dc2626}
@media (prefers-color-scheme:dark){:root{--bg:#0f111a;--card:#191c28;--text:#f3f4f6;--sub:#9ca3af;--line:#2a2e3d;--accent:#818cf8;--soft:#22264a;--ok:#4ade80;--bad:#f87171}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--text);font:15px/1.65 "Microsoft YaHei UI",system-ui,sans-serif}
header{position:sticky;top:0;background:var(--bg);padding:20px 16px 12px;border-bottom:1px solid var(--line);z-index:1}
.wrap{max-width:820px;margin:0 auto}h1{font-size:20px;margin:0 0 12px}
.bar{display:flex;gap:8px;flex-wrap:wrap}input{flex:1;min-width:180px;padding:9px 12px;border:1px solid var(--line);border-radius:10px;background:var(--card);color:var(--text);font:inherit}
button{padding:8px 14px;border:1px solid var(--line);border-radius:10px;background:var(--card);color:var(--text);font:inherit;cursor:pointer}button.on{background:var(--accent);border-color:var(--accent);color:#fff}
main{padding:8px 16px 60px}h2{font-size:14px;color:var(--sub);margin:28px 0 10px;font-weight:600}
.msg{display:flex;gap:12px;padding:6px 0}.time{flex:none;width:44px;color:var(--sub);font-size:12px;padding-top:3px}
.body{flex:1;min-width:0;white-space:pre-wrap;word-break:break-word}.u .body{color:var(--accent);font-weight:600}.tool .body{color:var(--sub);font-size:13px}.sys .body{color:var(--sub);font-size:13px;font-style:italic}
.task{background:var(--card);border:1px solid var(--line);border-radius:12px;padding:12px 14px;margin:8px 0}
.task .head{display:flex;gap:8px;align-items:center;flex-wrap:wrap;font-weight:600}.tag{font-size:12px;padding:1px 8px;border-radius:99px;background:var(--soft);color:var(--accent);font-weight:500}
.tag.done{color:var(--ok)}.tag.failed{color:var(--bad)}.task .meta{color:var(--sub);font-size:12px;margin:2px 0 6px}
details summary{cursor:pointer;color:var(--sub);font-size:13px}details div{white-space:pre-wrap;word-break:break-word;margin-top:6px;font-size:14px}
.empty{color:var(--sub);text-align:center;padding:60px 0}mark{background:#fde68a;color:inherit;border-radius:3px}
</style></head><body>
<header><div class="wrap"><h1>Aeri 历史记录</h1><div class="bar"><input id="q" placeholder="搜索对话和任务…" autofocus>
<button data-f="all" class="on">全部</button><button data-f="task">只看任务</button></div></div></header>
<main class="wrap" id="list"></main>
<script>
const DATA=__DATA__;let filter="all";
const esc=s=>String(s??"").replace(/[&<>"]/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;"}[c]));
const hl=(s,q)=>{s=esc(s);if(!q)return s;const i=q.replace(/[.*+?^${}()|[\]\\]/g,"\\$&");return s.replace(new RegExp(i,"gi"),m=>"<mark>"+m+"</mark>")};
const STATE={done:"已完成",failed:"失败",cancelled:"已取消"};
function render(){const q=document.getElementById("q").value.trim();const ql=q.toLowerCase();let html="";
for(const d of DATA){let rows="";for(const e of d.items){
 const hay=(e.text+" "+(e.task?e.task.description+" "+e.task.result:"")).toLowerCase();
 if(ql&&!hay.includes(ql))continue;if(filter==="task"&&e.kind!=="task")continue;
 if(e.kind==="task"){const t=e.task,m=Math.floor(t.secs/60),s=t.secs%60;
  rows+=`<div class="task"><div class="head">任务 #${t.id}<span class="tag ${t.state}">${STATE[t.state]||t.state}</span></div>
  <div class="meta">${e.t} · ${esc(t.executor)} · 用时 ${m?m+" 分 ":""}${s} 秒</div><div>${hl(t.description,q)}</div>
  ${t.result?`<details${q?" open":""}><summary>完整结果</summary><div>${hl(t.result,q)}</div></details>`:""}</div>`;}
 else{const c=["u","a","tool","sys"][e.role]||"a";rows+=`<div class="msg ${c}"><div class="time">${e.t.slice(0,5)}</div><div class="body">${e.role===0?"你：":""}${hl(e.text,q)}</div></div>`;}}
 if(rows)html+=`<h2>${d.day}</h2>`+rows;}
document.getElementById("list").innerHTML=html||'<div class="empty">没有找到记录</div>';}
document.getElementById("q").oninput=render;
for(const b of document.querySelectorAll("button[data-f]"))b.onclick=()=>{filter=b.dataset.f;for(const x of document.querySelectorAll("button[data-f]"))x.classList.toggle("on",x===b);render()};
render();
</script></body></html>"#;

// ---------- 给助手用的工具 ----------

pub fn definitions() -> Vec<Value> {
    vec![json!({
        "name": "history_search",
        "description": "查之前的对话和后台任务记录（保留 30 天）。用户问“我昨天让你做了什么”“上周那个任务结果是什么”“之前说过的 XX”时用。date 是 YYYY-MM-DD（先用 current_time 确认今天），query 是关键词；都不给就返回最近的记录。",
        "input_schema": {"type": "object", "properties": {
            "date": {"type": "string", "description": "YYYY-MM-DD，只看这一天"},
            "query": {"type": "string", "description": "关键词，匹配对话内容、任务要求和结果"},
            "tasks_only": {"type": "boolean", "description": "只看后台任务"}
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
