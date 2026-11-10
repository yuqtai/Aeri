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

use std::sync::Arc;

use serde_json::{Value, json};

use crate::config::Config;
use crate::reminders::{self, Reminders};
use crate::tasks::TaskManager;

pub struct Tools {
    pub apps: Arc<apps::AppIndex>,
    desktop: Option<Arc<desktop::Desktop>>,
    reader: Option<Arc<reader::Reader>>,
    browser: Option<Arc<browser::Browser>>,
    tasks: Option<Arc<TaskManager>>,
    reminders: Option<Arc<Reminders>>,
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
        }
    }

    /// 只有本地工具（启动时占位用）
    pub fn local_only(apps: Arc<apps::AppIndex>) -> Self {
        Self { apps, desktop: None, reader: None, browser: None, tasks: None, reminders: None }
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
        }
    }

    /// MCP 模式给外部智能体用：全套工具（桌面 + 浏览器，按设置里的开关），但没有后台任务工具
    pub fn for_agents(cfg: &Config, apps: Arc<apps::AppIndex>) -> Self {
        Self::new(cfg, apps)
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
        defs
    }

    /// 执行工具，返回（结果文字，是否出错）
    pub async fn call(&self, name: &str, input: &Value) -> (String, bool) {
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
                .unwrap_or_else(|| ("进程工具执行失败".into(), true));
        }
        if let Some(out) = files::call(name, input).await {
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
        (format!("未知或未启用的工具：{name}"), true)
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
