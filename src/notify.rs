//! Spoken alerts from coding agents the user runs themselves (Claude Code, Codex).
//!
//! Both agents run hook commands on events. We register `aeri.exe --notify claude|codex`
//! for "Stop" (finished) and "PermissionRequest" (waiting for approval). That short-lived
//! process reads the hook JSON from stdin, words an alert, and hands it to the running
//! Aeri over a localhost socket, which speaks it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::config;
use crate::locale::pick;
use crate::tasks::TaskDone;

/// Set on agents that Aeri starts itself; their completion is already announced.
pub const OWN_TASK_ENV: &str = "AERI_TASK";
const MARK: &str = "--notify";

fn port_file() -> PathBuf {
    config::app_dir().join("notify.json")
}

/// Running Aeri: listen on a random localhost port and pass alerts on to the assistant.
pub fn serve(tx: UnboundedSender<TaskDone>) {
    let Ok(listener) = TcpListener::bind("127.0.0.1:0") else { return };
    let Ok(addr) = listener.local_addr() else { return };
    let token = uuid::Uuid::new_v4().to_string();
    let _ = std::fs::write(port_file(), json!({"port": addr.port(), "token": token}).to_string());
    std::thread::Builder::new()
        .name("notify".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut line = String::new();
                if BufReader::new(stream.take(16 * 1024)).read_line(&mut line).is_err() {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                if v["token"].as_str() != Some(token.as_str()) {
                    continue;
                }
                let text = v["text"].as_str().unwrap_or_default();
                let finished = v["event"].as_str() == Some("Stop");
                if !text.is_empty() && take_watch(v["agent"].as_str().unwrap_or_default(), v["project"].as_str().unwrap_or_default(), finished) {
                    let _ = tx.send(TaskDone { id: 0, ok: true, text: text.to_string(), reminder: false, notice: true });
                }
            }
        })
        .ok();
}

/// `aeri.exe --notify <claude|codex>` run by an agent hook. Always exits quietly so the
/// agent is never blocked or changed by it.
pub fn run_hook(agent: &str) {
    if std::env::var_os(OWN_TASK_ENV).is_some() {
        return;
    }
    let mut input = String::new();
    let _ = std::io::stdin().take(1024 * 1024).read_to_string(&mut input);
    let v: Value = serde_json::from_str(input.trim_start_matches('\u{feff}')).unwrap_or(Value::Null);
    crate::locale::apply_preference(&config::load().language);
    let debug = std::env::var_os("AERI_NOTIFY_DEBUG").is_some();
    if debug { eprintln!("input={input:?}"); }
    let Some(text) = alert_text(agent, &v) else { return };
    let project = project_of(&v);
    let event = v["hook_event_name"].as_str().unwrap_or_default().to_string();
    if debug { eprintln!("text={text} file={}", port_file().display()); }
    let Ok(info) = std::fs::read_to_string(port_file()) else { return };
    let Ok(info) = serde_json::from_str::<Value>(&info) else { return };
    let Some(port) = info["port"].as_u64() else { return };
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port as u16));
    if let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) {
        let msg = json!({"token": info["token"], "text": text, "agent": agent, "project": project, "event": event}).to_string() + "\n";
        let _ = s.write_all(msg.as_bytes());
    }
}

fn project_of(v: &Value) -> String {
    v["cwd"]
        .as_str()
        .and_then(|c| std::path::Path::new(c.trim_end_matches(['/', '\\'])).file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

// ---------- watches: only speak up when the user asked ----------

struct Watch {
    /// "claude", "codex", or "" for either
    agent: String,
    /// Part of the project folder name (lowercase), or "" for any session
    project: String,
    since: std::time::Instant,
}

static WATCHES: std::sync::Mutex<Vec<Watch>> = std::sync::Mutex::new(Vec::new());
const WATCH_HOURS: u64 = 12;

/// Is someone waiting for this event? A finished session ends the watch that matched it.
fn take_watch(agent: &str, project: &str, finished: bool) -> bool {
    let mut ws = WATCHES.lock().unwrap();
    ws.retain(|w| w.since.elapsed() < Duration::from_secs(WATCH_HOURS * 3600));
    let project = project.to_lowercase();
    let Some(i) = ws
        .iter()
        .position(|w| (w.agent.is_empty() || w.agent == agent) && (w.project.is_empty() || project.contains(&w.project)))
    else {
        return false;
    };
    if finished {
        ws.remove(i);
    }
    true
}

pub fn definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "agent_watch",
            "description": "Keep an eye on a Claude Code or Codex session the user runs themselves, e.g. \"watch Claude and tell me when it's done\". Aeri then says when that session finishes (the watch ends) or waits for approval. Only use when the user asks.",
            "input_schema": {"type": "object", "properties": {
                "agent": {"type": "string", "enum": ["claude", "codex", "any"]},
                "project": {"type": "string", "description": "Project folder name if the user named one; omit for any session"}
            }, "required": ["agent"]}
        }),
        json!({
            "name": "agent_watch_cancel",
            "description": "Stop watching Claude Code / Codex sessions.",
            "input_schema": {"type": "object", "properties": {}}
        }),
    ]
}

pub fn call(name: &str, input: &Value) -> Option<(String, bool)> {
    match name {
        "agent_watch" => {
            let agent = match input["agent"].as_str().unwrap_or("any") {
                "claude" => "claude",
                "codex" => "codex",
                _ => "",
            };
            let project = input["project"].as_str().unwrap_or_default().trim().to_lowercase();
            let note = match install(true) {
                Ok(n) => n,
                Err(e) => return Some((e, true)),
            };
            let mut ws = WATCHES.lock().unwrap();
            ws.retain(|w| !(w.agent == agent && w.project == project));
            ws.push(Watch { agent: agent.into(), project: project.clone(), since: std::time::Instant::now() });
            let who = match agent {
                "claude" => "Claude",
                "codex" => "Codex",
                _ => "Claude / Codex",
            };
            let wher = if project.is_empty() { String::new() } else { format!(" ({project})") };
            Some((format!("Watching {who}{wher}; Aeri will speak up when it finishes or needs approval ({note})."), false))
        }
        "agent_watch_cancel" => {
            WATCHES.lock().unwrap().clear();
            Some(("Stopped watching.".into(), false))
        }
        _ => None,
    }
}

/// What to say for a hook event, or None for events we stay quiet about.
fn alert_text(agent: &str, v: &Value) -> Option<String> {
    let name = if agent == "codex" { "Codex" } else { "Claude" };
    let project = project_of(v);
    let tool = v["tool_name"].as_str().unwrap_or_default();
    match v["hook_event_name"].as_str()? {
        "Stop" if v["stop_hook_active"].as_bool() != Some(true) => Some(if project.is_empty() {
            pick(&format!("{name} has finished."), &format!("{name} 做完了。")).to_string()
        } else {
            pick(&format!("{name} has finished in {project}."), &format!("{project} 里的 {name} 做完了。")).to_string()
        }),
        "PermissionRequest" => {
            let what = if tool.is_empty() { String::new() } else { format!(" ({tool})") };
            Some(if project.is_empty() {
                pick(&format!("{name} needs your approval{what}."), &format!("{name} 在等你批准{what}。")).to_string()
            } else {
                pick(&format!("{name} in {project} needs your approval{what}."), &format!("{project} 里的 {name} 在等你批准{what}。")).to_string()
            })
        }
        _ => None,
    }
}

// ---------- installing the hooks ----------

const EVENTS: [&str; 2] = ["Stop", "PermissionRequest"];

fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").map(PathBuf::from)
}

fn exe() -> String {
    std::env::current_exe().map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default()
}

fn ours(entry: &Value) -> bool {
    entry["hooks"].as_array().is_some_and(|hs| {
        hs.iter().any(|h| {
            let cmd = h["command"].as_str().unwrap_or_default();
            let args = h["args"].to_string();
            (cmd.contains("aeri") || cmd.contains("Aeri")) && (cmd.contains(MARK) || args.contains(MARK))
        })
    })
}

/// Add (on) or remove (off) our hook entries in one agent's hooks JSON, leaving everything else alone.
fn update_hooks(root: &mut Value, entry: Option<Value>) {
    if !root.is_object() {
        *root = json!({});
    }
    if !root["hooks"].is_object() {
        root["hooks"] = json!({});
    }
    for ev in EVENTS {
        let list = &mut root["hooks"][ev];
        let mut items: Vec<Value> = list.as_array().cloned().unwrap_or_default();
        items.retain(|e| !ours(e));
        if let Some(e) = &entry {
            items.push(e.clone());
        }
        if items.is_empty() {
            if let Some(map) = root["hooks"].as_object_mut() {
                map.remove(ev);
            }
        } else {
            *list = Value::Array(items);
        }
    }
}

/// Returns whether the file changed.
fn edit_json(path: &PathBuf, entry: Option<Value>) -> Result<bool, String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut root: Value = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?
    };
    let before = root.clone();
    update_hooks(&mut root, entry);
    if root == before {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    if !text.is_empty() {
        let _ = std::fs::write(path.with_extension("json.aeri-bak"), &text);
    }
    let out = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())? + "\n";
    std::fs::write(path, out).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(true)
}

/// Make sure both agents call us on Stop / PermissionRequest. The hook stays quiet unless a watch is active.
/// Ok carries a note saying whether the hook was just added (then only newly started sessions report).
pub fn install(on: bool) -> Result<String, String> {
    let Some(home) = home() else { return Err("no home folder".into()) };
    let exe = exe();
    let mut added = Vec::new();
    let mut found = false;
    let claude_dir = home.join(".claude");
    if claude_dir.exists() {
        found = true;
        let entry = on.then(|| json!({"hooks": [{"type": "command", "command": exe, "args": [MARK, "claude"], "timeout": 5}]}));
        if edit_json(&claude_dir.join("settings.json"), entry)? {
            added.push("Claude Code");
        }
    }
    let codex_dir = home.join(".codex");
    if codex_dir.exists() {
        found = true;
        let cmd = format!("& '{exe}' {MARK} codex");
        let entry = on.then(|| json!({"hooks": [{"type": "command", "command": cmd, "commandWindows": cmd, "timeout": 5}]}));
        if edit_json(&codex_dir.join("hooks.json"), entry)? {
            added.push("Codex");
        }
    }
    if !found {
        return Err("Neither Claude Code nor Codex is installed on this PC.".into());
    }
    Ok(if added.is_empty() {
        "hooks already in place".into()
    } else {
        format!("hook just added for {}; sessions that were already open must be restarted before they report", added.join(" and "))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_alerts() {
        let stop = json!({"hook_event_name": "Stop", "cwd": "D:\\code\\shop"});
        let t = alert_text("claude", &stop).unwrap();
        assert!(t.contains("Claude") && t.contains("shop"), "{t}");
        let perm = json!({"hook_event_name": "PermissionRequest", "cwd": "D:/x/api", "tool_name": "Bash"});
        let t = alert_text("codex", &perm).unwrap();
        assert!(t.contains("Codex") && t.contains("api") && t.contains("Bash"), "{t}");
        assert!(alert_text("claude", &json!({"hook_event_name": "PreToolUse"})).is_none());
        assert!(alert_text("claude", &json!({"hook_event_name": "Stop", "stop_hook_active": true})).is_none());
    }

    #[test]
    fn speaks_only_while_watched() {
        assert!(!take_watch("claude", "shop", true));
        WATCHES.lock().unwrap().push(Watch { agent: "claude".into(), project: "shop".into(), since: std::time::Instant::now() });
        assert!(!take_watch("codex", "shop", true));
        assert!(!take_watch("claude", "blog", true));
        assert!(take_watch("claude", "Shop-API", false)); // approval: still watching
        assert!(take_watch("claude", "shop", true)); // finished: the watch ends
        assert!(!take_watch("claude", "shop", true));
    }

    #[test]
    fn installs_and_removes_only_our_hooks() {
        let other = json!({"hooks": [{"type": "command", "command": "C:/other/hook.exe"}]});
        let mut root = json!({"model": "x", "hooks": {"Stop": [other.clone()]}});
        let ours_entry = json!({"hooks": [{"type": "command", "command": "C:/A/aeri.exe", "args": ["--notify", "claude"]}]});
        update_hooks(&mut root, Some(ours_entry.clone()));
        update_hooks(&mut root, Some(ours_entry.clone())); // twice: still one copy
        assert_eq!(root["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert_eq!(root["hooks"]["PermissionRequest"].as_array().unwrap().len(), 1);
        update_hooks(&mut root, None);
        assert_eq!(root["hooks"]["Stop"], json!([other]));
        assert!(root["hooks"].get("PermissionRequest").is_none());
        assert_eq!(root["model"], "x");
    }
}
