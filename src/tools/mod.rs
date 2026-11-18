//! 工具注册表：本地工具 + 文件 + 系统 + 提醒 + 桌面控制（agent-desktop）+ 读屏 + 浏览器控制。
//! - 前台对话用全套工具再加上后台任务（开始 / 进度 / 取消）：几步能做完的自己做，耗时长的才交出去；
//! - 内置后台助手和 MCP（给 Codex / Claude Code）用全套工具，不带后台任务工具。
//! 前台和内置后台助手共用同一个桌面控制、读屏线程和浏览器会话。

pub mod apps;
pub mod browser;
pub mod desktop;
pub mod files;
pub mod process;
pub mod ps;
pub mod reader;
pub mod system;

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::config::Config;
use crate::reminders::{self, Reminders};
use crate::tasks::TaskManager;

fn readonly_denied() -> String {
    crate::locale::pick(
        "Current background permission is set to \"read-only\"; modifying or controlling actions are not allowed",
        "当前后台权限设为“只读”，不允许执行修改或控制类操作",
    )
    .into()
}

const READONLY_BLOCKED: &[&str] = &[
    "launch_app",
    "open_url",
    "web_search",
    "close_app",
    "file_rename",
    "file_move",
    "file_delete",
    "file_open",
    "volume_set",
    "media_control",
    "system_setting",
    "screenshot",
    "power",
    "window_arrange",
    "clipboard_write",
    "desktop_click",
    "desktop_double_click",
    "desktop_set_value",
    "desktop_select",
    "desktop_check",
    "desktop_type",
    "desktop_keys",
    "desktop_focus",
    "desktop_scroll",
    "reminder_add",
    "reminder_cancel",
    "agent_watch",
    "agent_watch_cancel",
    "app_update",
    "browser_click",
    "browser_type",
    "browser_fill_form",
    "browser_select_option",
    "browser_press_key",
    "browser_file_upload",
    "browser_handle_dialog",
    "browser_evaluate",
    "start_task",
    "resume_task",
    "cancel_task",
];

fn is_readonly_blocked(name: &str) -> bool {
    READONLY_BLOCKED.contains(&name)
}

pub struct Tools {
    pub apps: Arc<apps::AppIndex>,
    desktop: Option<Arc<desktop::Desktop>>,
    reader: Option<Arc<reader::Reader>>,
    browser: Option<Arc<browser::Browser>>,
    tasks: Option<Arc<TaskManager>>,
    reminders: Option<Arc<Reminders>>,
    access: String,
    workdir: Option<PathBuf>,
}

impl Tools {
    /// 全套工具
    pub fn new(cfg: &Config, apps: Arc<apps::AppIndex>) -> Self {
        system::start_clipboard_watch();
        Self {
            apps,
            desktop: cfg.desktop_control.then(|| Arc::new(desktop::Desktop::new())),
            reader: cfg.desktop_control.then(|| Arc::new(reader::Reader::new())),
            browser: cfg.browser_control.then(|| browser::Browser::new(&cfg.playwright_mcp, cfg.browser_idle_minutes)),
            tasks: None,
            reminders: None,
            access: "full".into(),
            workdir: None,
        }
    }

    /// 只有本地工具（启动时占位用）
    pub fn local_only(apps: Arc<apps::AppIndex>) -> Self {
        Self {
            apps,
            desktop: None,
            reader: None,
            browser: None,
            tasks: None,
            reminders: None,
            access: "full".into(),
            workdir: None,
        }
    }

    /// 前台工具：同一套工具再加上后台任务和提醒
    pub fn with_tasks(&self, tasks: Arc<TaskManager>, reminders: Arc<Reminders>) -> Self {
        Self {
            apps: self.apps.clone(),
            desktop: self.desktop.clone(),
            reader: self.reader.clone(),
            browser: self.browser.clone(),
            tasks: Some(tasks),
            reminders: Some(reminders),
            access: self.access.clone(),
            workdir: self.workdir.clone(),
        }
    }

    /// 为后台智能体或 MCP 设置权限边界（full / workspace / readonly）与工作目录
    pub fn with_access(&self, access: &str, workdir: PathBuf) -> Self {
        let norm = match access.trim() {
            "readonly" => "readonly",
            "workspace" => "workspace",
            _ => "full",
        };
        Self {
            apps: self.apps.clone(),
            desktop: self.desktop.clone(),
            reader: self.reader.clone(),
            browser: self.browser.clone(),
            tasks: self.tasks.clone(),
            reminders: self.reminders.clone(),
            access: norm.into(),
            workdir: Some(workdir),
        }
    }

    /// MCP 模式给外部智能体用：全套工具（桌面 + 浏览器，按设置里的开关），但没有后台任务工具
    pub fn for_agents(cfg: &Config, apps: Arc<apps::AppIndex>) -> Self {
        Self::new(cfg, apps).with_access(&cfg.agent_access, cfg.agent_dir())
    }

    pub fn definitions(&self) -> Vec<Value> {
        let mut defs = apps::definitions();
        defs.extend(process::definitions());
        defs.extend(files::definitions());
        defs.extend(system::definitions());
        defs.extend(crate::plugins::definitions());
        if self.tasks.is_some() {
            defs.extend(task_definitions());
        }
        if self.reminders.is_some() {
            defs.extend(reminders::definitions());
            defs.extend(crate::notify::definitions());
            defs.extend(crate::history::definitions());
            defs.extend(crate::plugins::market_definitions());
            defs.push(crate::update::definition());
        }
        if self.desktop.is_some() {
            defs.extend(desktop::definitions());
        }
        if self.reader.is_some() {
            defs.extend(reader::definitions());
        }
        if self.browser.is_some() {
            defs.extend(browser::definitions());
        }
        if self.access == "readonly" {
            defs.retain(|d| !d["name"].as_str().is_some_and(is_readonly_blocked));
            for d in &mut defs {
                match d["name"].as_str() {
                    Some("clipboard_history") => {
                        d["description"] = json!("Read recently copied text (recorded since Aeri started, up to 20 entries, newest first).");
                        if let Some(props) = d["input_schema"]["properties"].as_object_mut() {
                            props.remove("restore");
                        }
                    }
                    Some("local_http") => {
                        d["description"] = json!("Read a local software HTTP API via GET on this computer (127.0.0.1 / localhost only), following its plugin guide.");
                        if let Some(props) = d["input_schema"]["properties"].as_object_mut() {
                            props.insert("method".into(), json!({"type": "string", "enum": ["GET"]}));
                            props.remove("body");
                        }
                        d["input_schema"]["required"] = json!(["method", "url"]);
                    }
                    Some("plugin_market") => {
                        d["description"] = json!("List available and installed software plugins in the plugin market.");
                        if let Some(props) = d["input_schema"]["properties"].as_object_mut() {
                            props.insert("action".into(), json!({"type": "string", "enum": ["list"]}));
                            props.remove("id");
                        }
                    }
                    Some("browser_snapshot" | "browser_find") => {
                        if let Some(props) = d["input_schema"]["properties"].as_object_mut() {
                            props.remove("filename");
                        }
                    }
                    _ => {}
                }
            }
        }
        defs
    }

    /// 执行工具，返回（结果文字，是否出错）
    pub async fn call(&self, name: &str, input: &Value) -> (String, bool) {
        if self.access == "readonly" {
            let restore_clip = name == "clipboard_history" && input.get("restore").is_some_and(|v| !v.is_null());
            let mutating_http = name == "local_http"
                && (!input["method"].as_str().is_some_and(|m| m.eq_ignore_ascii_case("GET"))
                    || input.get("body").is_some_and(|v| !v.is_null()));
            let mutating_market = name == "plugin_market"
                && input["action"].as_str().unwrap_or("list") != "list";
            let browser_write_file = matches!(name, "browser_snapshot" | "browser_find")
                && input.get("filename").and_then(Value::as_str).is_some_and(|s| !s.trim().is_empty());
            if is_readonly_blocked(name) || restore_clip || mutating_http || mutating_market || browser_write_file {
                return (readonly_denied(), true);
            }
        }
        if self.access == "workspace" {
            if name == "power" {
                return (
                    crate::locale::pick(
                        "Current permission is set to \"workspace only\"; power actions are not allowed",
                        "当前权限设为“只改工作目录”，不允许执行电源控制操作",
                    )
                    .into(),
                    true,
                );
            }
            if name == "app_update" && input["action"].as_str() == Some("install") {
                return (
                    crate::locale::pick(
                        "Current permission is set to \"workspace only\"; installing software updates is not allowed",
                        "当前权限设为“只改工作目录”，不允许安装软件更新",
                    )
                    .into(),
                    true,
                );
            }
            if name == "plugin_market" && input["action"].as_str().unwrap_or("list") != "list" {
                return (
                    crate::locale::pick(
                        "Current permission is set to \"workspace only\"; installing or removing global plugins is not allowed",
                        "当前权限设为“只改工作目录”，不允许安装或卸载全局插件",
                    )
                    .into(),
                    true,
                );
            }
        }
        if let Some(tasks) = &self.tasks {
            match name {
                "start_task" => return tasks.start(input["task"].as_str().unwrap_or("")),
                "resume_task" => return tasks.resume(input["id"].as_u64().and_then(|id| u32::try_from(id).ok()), input["instruction"].as_str().unwrap_or("")),
                "task_status" => return (tasks.status(), false),
                "cancel_task" => return tasks.cancel(input["id"].as_u64().map(|i| i as u32)),
                _ => {}
            }
        }
        if let Some(out) = apps::call(&self.apps, name, input) {
            return out;
        }
        if matches!(name, "close_app" | "list_processes") {
            // 关闭程序要等它退出（最多几秒），放到阻塞线程里
            let (apps, name, input) = (self.apps.clone(), name.to_string(), input.clone());
            return tokio::task::spawn_blocking(move || process::call(&apps, &name, &input))
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| (crate::locale::pick("Process tool failed", "进程工具执行失败").into(), true));
        }
        let ws_limit = (self.access == "workspace").then_some(self.workdir.as_deref()).flatten();
        if let Some(out) = files::call_with_limit(name, input, ws_limit).await {
            return out;
        }
        if let Some(out) = system::call(name, input).await {
            return out;
        }
        // 插件市场只给前台（有提醒工具的就是前台）
        if name != "plugin_market" || self.reminders.is_some() {
            if let Some(out) = crate::plugins::call(name, input).await {
                return out;
            }
        }
        if let Some(r) = &self.reminders {
            if let Some(out) = r.call(name, input) {
                return out;
            }
            if let Some(out) = crate::notify::call(name, input) {
                return out;
            }
            if let Some(out) = crate::history::call(name, input) {
                return out;
            }
            if name == "app_update" {
                return crate::update::call(input).await;
            }
        }
        if name == "screen_read" {
            if let Some(r) = &self.reader {
                return r.call(input).await;
            }
        }
        if name.starts_with("desktop_") || name.starts_with("clipboard_") {
            if let Some(d) = &self.desktop {
                return d.call(name, input).await;
            }
        }
        if name.starts_with("browser_") {
            if let Some(b) = &self.browser {
                return b.call(name, input).await;
            }
        }
        (
            crate::locale::pick(
                &format!("Unknown or disabled tool: {name}"),
                &format!("未知或未启用的工具：{name}"),
            )
            .to_string(),
            true,
        )
    }
}

fn task_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "resume_task",
            "description": "Continue, retry or modify a finished background task with its original request and complete result. Omit id for the latest task. Prefer this when the user asks to continue or revise a previous result. Cancel running tasks before changing them.",
            "input_schema": {"type": "object", "properties": {"id": {"type": "integer", "minimum": 1}, "instruction": {"type": "string"}}, "required": ["instruction"]}
        }),
        json!({
            "name": "start_task",
            "description": "Delegate a multi-step task to a background agent (software or web operations, research and summaries, reading/writing or organizing files). Returns immediately. task must be a complete standalone instruction including the key points of the original request.",
            "input_schema": {"type": "object", "properties": {"task": {"type": "string"}}, "required": ["task"]}
        }),
        json!({
            "name": "task_status",
            "description": "Check background task progress.",
            "input_schema": {"type": "object", "properties": {}}
        }),
        json!({
            "name": "cancel_task",
            "description": "Cancel a background task. Omit id to cancel the most recent running task.",
            "input_schema": {"type": "object", "properties": {"id": {"type": "integer"}}}
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn readonly_and_workspace_permissions() {
        let apps = Arc::new(apps::AppIndex::load_cached());
        let base = Tools::local_only(apps);

        let ro = base.with_access("readonly", PathBuf::from("."));
        let ro_names: Vec<String> = ro.definitions().into_iter().filter_map(|d| d["name"].as_str().map(str::to_string)).collect();
        assert!(ro_names.contains(&"calc".to_string()));
        assert!(ro_names.contains(&"file_find".to_string()));
        assert!(ro_names.contains(&"clipboard_history".to_string()));
        assert!(ro_names.contains(&"plugin_guide".to_string()));
        assert!(ro_names.contains(&"local_http".to_string()));
        assert!(!ro_names.contains(&"file_delete".to_string()));
        assert!(!ro_names.contains(&"launch_app".to_string()));
        assert!(!ro_names.contains(&"power".to_string()));

        let (msg, err) = ro.call("file_delete", &json!({"path": "C:\\test.txt"})).await;
        assert!(err && msg == readonly_denied());
        let (msg, err) = ro.call("clipboard_history", &json!({"restore": 1})).await;
        assert!(err && msg == readonly_denied());
        let (msg, err) = ro.call("local_http", &json!({"url": "http://127.0.0.1:9/test"})).await;
        assert!(err && msg == readonly_denied(), "default POST must be blocked in readonly");
        let (msg, err) = ro.call("local_http", &json!({"method": "POST", "url": "http://127.0.0.1:9/test"})).await;
        assert!(err && msg == readonly_denied());
        let (msg, err) = ro.call("plugin_market", &json!({"action": "install", "id": "siyuan"})).await;
        assert!(err && msg == readonly_denied());
        let (_, err) = ro.call("plugin_guide", &json!({"id": "notepad"})).await;
        assert!(!err);
        let (_, err) = ro.call("calc", &json!({"expression": "1+1"})).await;
        assert!(!err);

        let root = std::env::temp_dir().join(format!("aeri-tools-ws-{}", uuid::Uuid::new_v4()));
        let ws_dir = root.join("work");
        let out_dir = root.join("other");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::create_dir_all(&out_dir).unwrap();
        let outside_file = out_dir.join("x.txt");
        let inside_file = ws_dir.join("y.txt");
        std::fs::write(&outside_file, b"x").unwrap();
        std::fs::write(&inside_file, b"y").unwrap();

        let ws = base.with_access("workspace", ws_dir.clone());
        let (_, err) = ws.call("power", &json!({"action": "shutdown", "confirmed": true})).await;
        assert!(err);
        let (_, err) = ws.call("app_update", &json!({"action": "install"})).await;
        assert!(err);
        let (_, err) = ws.call("plugin_market", &json!({"action": "install", "id": "siyuan"})).await;
        assert!(err);
        let (msg, err) = ws.call("file_rename", &json!({"path": outside_file.to_str().unwrap(), "new_name": "z"})).await;
        assert!(err && msg.contains(crate::locale::pick("workspace only", "只改工作目录")));
        let (_, err) = ws.call("file_rename", &json!({"path": inside_file.to_str().unwrap(), "new_name": "z"})).await;
        assert!(!err && ws_dir.join("z.txt").exists());

        std::fs::remove_dir_all(&root).unwrap();
    }
}
