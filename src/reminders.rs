//! 提醒和倒计时：到点后通过和后台任务相同的通道交给调度中心，由助手主动说出来。
//! 保存在 %APPDATA%\VoiceAssistant\reminders.json，Aeri 重启后还在；关机期间错过的，启动时马上提醒。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToFileTime, SystemTimeToTzSpecificLocalTime, TzSpecificLocalTimeToSystemTime};

use crate::config;
use crate::tasks::TaskDone;

#[derive(Clone, Serialize, Deserialize)]
struct Reminder {
    id: u32,
    /// 到点时间（Unix 秒）
    due: u64,
    text: String,
}

pub struct Reminders {
    list: Mutex<Vec<Reminder>>,
    tx: UnboundedSender<TaskDone>,
    /// 测试时不写文件，免得覆盖真正的提醒
    persist: bool,
}

pub fn definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "reminder_add",
            "description": "Set a reminder or countdown; Aeri announces it when due. For remind me to drink water in ten minutes use minutes=10; for 3 PM use at=\"15:00\"; for tomorrow at 9 AM use at=\"tomorrow-date 09:00\" in YYYY-MM-DD HH:MM format (first use current_time to check today). text is the announcement, for example Countdown finished.",
            "input_schema": {"type": "object", "properties": {
                "minutes": {"type": "number", "description": "Minutes from now, including fractions (0.5 = 30 seconds)."},
                "at": {"type": "string", "description": "HH:MM (today, or tomorrow if already past) or YYYY-MM-DD HH:MM."},
                "text": {"type": "string"}
            }, "required": ["text"]}
        }),
        json!({
            "name": "reminder_list",
            "description": "List reminders that are not yet due.",
            "input_schema": {"type": "object", "properties": {}}
        }),
        json!({
            "name": "reminder_cancel",
            "description": "Cancel a reminder by id, or omit id to cancel all reminders.",
            "input_schema": {"type": "object", "properties": {"id": {"type": "integer"}}}
        }),
    ]
}

impl Reminders {
    pub fn new(tx: UnboundedSender<TaskDone>) -> Arc<Self> {
        let list: Vec<Reminder> = std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let me = Arc::new(Self { list: Mutex::new(Vec::new()), tx, persist: true });
        for r in list {
            me.schedule(r);
        }
        me
    }

    pub fn call(self: &Arc<Self>, name: &str, input: &Value) -> Option<(String, bool)> {
        let r = match name {
            "reminder_add" => self.add(input["minutes"].as_f64(), input["at"].as_str().unwrap_or(""), input["text"].as_str().unwrap_or("")),
            "reminder_list" => Ok(self.describe()),
            "reminder_cancel" => self.cancel(input["id"].as_u64().map(|i| i as u32)),
            _ => return None,
        };
        Some(match r {
            Ok(t) => (t, false),
            Err(t) => (t, true),
        })
    }

    pub fn add(self: &Arc<Self>, minutes: Option<f64>, at: &str, text: &str) -> Result<String, String> {
        let text = text.trim();
        let text = if text.is_empty() { crate::locale::pick("Time is up", "时间到了") } else { text };
        let now = now_secs();
        let due = match (minutes, at.trim()) {
            (Some(m), _) if m > 0.0 => now + (m * 60.0).round().max(1.0) as u64,
            (_, at) if !at.is_empty() => parse_at(at)?,
            _ => return Err(crate::locale::pick("Provide minutes or at", "要给出 minutes 或 at").into()),
        };
        if due <= now {
            return Err(crate::locale::pick("That time is already in the past", "这个时间已经过去了").into());
        }
        let id = self.list.lock().unwrap().iter().map(|r| r.id).max().unwrap_or(0) + 1;
        self.schedule(Reminder { id, due, text: text.to_string() });
        Ok(crate::locale::pick(&format!("Reminder set for {} ({}): {text}, id {id}", when_text(due), local_text(due)), &format!("好的，{}（{}）提醒：{text}，编号 {id}", when_text(due), local_text(due))).to_string())
    }

    fn schedule(self: &Arc<Self>, r: Reminder) {
        self.list.lock().unwrap().push(r.clone());
        self.save();
        let me = self.clone();
        tokio::spawn(async move {
            let wait = r.due.saturating_sub(now_secs());
            tokio::time::sleep(Duration::from_secs(wait)).await;
            // 期间可能被取消
            let still = {
                let mut list = me.list.lock().unwrap();
                let before = list.len();
                list.retain(|x| x.id != r.id);
                list.len() != before
            };
            if still {
                me.save();
                crate::sys::beep(988, 180);
                let late = now_secs().saturating_sub(r.due);
                let text = if late > 120 { crate::locale::pick(&format!("{} (originally due at {}; Aeri was not running then)", r.text, local_text(r.due)), &format!("{}（本来是 {} 提醒，当时 Aeri 没在运行）", r.text, local_text(r.due))).to_string() } else { r.text };
                let _ = me.tx.send(TaskDone { id: r.id, ok: true, text, reminder: true, notice: false });
            }
        });
    }

    fn describe(&self) -> String {
        let mut list = self.list.lock().unwrap().clone();
        if list.is_empty() {
            return crate::locale::pick("There are no reminders", "现在没有提醒").into();
        }
        list.sort_by_key(|r| r.due);
        list.iter().map(|r| format!("{}. {}（{}）：{}", r.id, local_text(r.due), when_text(r.due), r.text)).collect::<Vec<_>>().join("\n")
    }

    fn cancel(&self, id: Option<u32>) -> Result<String, String> {
        let msg = {
            let mut list = self.list.lock().unwrap();
            match id {
                Some(id) => {
                    let before = list.len();
                    list.retain(|r| r.id != id);
                    if list.len() == before {
                        return Err(crate::locale::pick(&format!("There is no reminder with id {id}"), &format!("没有编号 {id} 的提醒")).to_string());
                    }
                    crate::locale::pick(&format!("Cancelled reminder {id}"), &format!("已取消提醒 {id}")).to_string()
                }
                None => {
                    let n = list.len();
                    list.clear();
                    crate::locale::pick(&format!("Cancelled all {n} reminders"), &format!("已取消全部 {n} 个提醒")).to_string()
                }
            }
        };
        self.save();
        Ok(msg)
    }

    fn save(&self) {
        if !self.persist {
            return;
        }
        let list = self.list.lock().unwrap().clone();
        if let Ok(b) = serde_json::to_vec_pretty(&list) {
            let _ = std::fs::write(path(), b);
        }
    }
}

fn path() -> PathBuf {
    config::app_dir().join("reminders.json")
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// “HH:MM”（今天，已过就明天）或“YYYY-MM-DD HH:MM”，本地时间 → Unix 秒
fn parse_at(at: &str) -> Result<u64, String> {
    let bad = || crate::locale::pick(&format!("Invalid time: {at}; use HH:MM or YYYY-MM-DD HH:MM"), &format!("时间格式不对：「{at}」，要写成 HH:MM 或 YYYY-MM-DD HH:MM")).to_string();
    let at = at.replace('：', ":").replace('/', "-").replace('T', " ");
    let (date, time) = match at.trim().split_once(' ') {
        Some((d, t)) => (Some(d.trim().to_string()), t.trim().to_string()),
        None => (None, at.trim().to_string()),
    };
    let (h, m) = time.split_once(':').ok_or_else(bad)?;
    let (h, m): (u16, u16) = (h.parse().map_err(|_| bad())?, m.get(..2).unwrap_or(m).parse().map_err(|_| bad())?);
    if h > 23 || m > 59 {
        return Err(bad());
    }
    let mut st = unsafe { GetLocalTime() };
    if let Some(d) = &date {
        let parts: Vec<u16> = d.split('-').filter_map(|p| p.parse().ok()).collect();
        let [y, mo, day] = parts[..] else { return Err(bad()) };
        (st.wYear, st.wMonth, st.wDay) = (y, mo, day);
    }
    (st.wHour, st.wMinute, st.wSecond, st.wMilliseconds) = (h, m, 0, 0);
    let mut due = local_to_unix(&st).ok_or_else(bad)?;
    if date.is_none() && due <= now_secs() {
        due += 24 * 3600;
    }
    Ok(due)
}

fn local_to_unix(local: &SYSTEMTIME) -> Option<u64> {
    unsafe {
        let mut utc = SYSTEMTIME::default();
        TzSpecificLocalTimeToSystemTime(None, local, &mut utc).ok()?;
        let mut ft = FILETIME::default();
        SystemTimeToFileTime(&utc, &mut ft).ok()?;
        let ticks = ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64;
        Some(ticks / 10_000_000 - 11_644_473_600)
    }
}

fn local_text(unix: u64) -> String {
    let ticks = (unix + 11_644_473_600) * 10_000_000;
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    unsafe {
        if FileTimeToSystemTime(&ft, &mut utc).is_err() || SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_err() {
            return String::new();
        }
    }
    let today = unsafe { GetLocalTime() };
    if (local.wYear, local.wMonth, local.wDay) == (today.wYear, today.wMonth, today.wDay) {
        format!("{:02}:{:02}", local.wHour, local.wMinute)
    } else {
        crate::locale::pick(&format!("{}/{}, {:02}:{:02}", local.wMonth, local.wDay, local.wHour, local.wMinute), &format!("{}月{}日 {:02}:{:02}", local.wMonth, local.wDay, local.wHour, local.wMinute)).to_string()
    }
}

fn when_text(due: u64) -> String {
    let s = due.saturating_sub(now_secs());
    match s {
        0..60 => crate::locale::pick(&format!("in {s} seconds"), &format!("{s} 秒后")).to_string(),
        60..3600 => crate::locale::pick(&format!("in {} minutes", (s + 30) / 60), &format!("{} 分钟后", (s + 30) / 60)).to_string(),
        _ => crate::locale::pick(&format!("in {} hours {} minutes", s / 3600, (s % 3600 + 30) / 60), &format!("{} 小时 {} 分钟后", s / 3600, (s % 3600 + 30) / 60)).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_times() {
        let now = now_secs();
        let t = parse_at("23:59").unwrap();
        assert!(t > now && t <= now + 24 * 3600);
        assert!(parse_at("25:00").is_err());
        assert!(parse_at("下午三点").is_err());
        let d = parse_at("2030-01-02 09:30").unwrap();
        assert!(local_text(d).contains(crate::locale::pick("1/2, 09:30", "1月2日 09:30")));
        assert_eq!(local_to_unix(&unsafe { GetLocalTime() }).unwrap().abs_diff(now) < 3, true);
    }

    #[tokio::test]
    async fn fires() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let r = Arc::new(Reminders { list: Mutex::new(Vec::new()), tx, persist: false });
        r.schedule(Reminder { id: 1, due: now_secs() + 1, text: "喝水".into() });
        r.schedule(Reminder { id: 2, due: now_secs() + 1, text: "取消的".into() });
        r.cancel(Some(2)).unwrap();
        let got = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
        assert!(got.reminder && got.text == "喝水");
        assert!(tokio::time::timeout(Duration::from_millis(1500), rx.recv()).await.is_err());
    }
}
