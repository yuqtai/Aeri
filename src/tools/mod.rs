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
        if self.tasks.is_some() {
            defs.extend(task_definitions());
        }
        if self.reminders.is_some() {
            defs.extend(reminders::definitions());
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
        if let Some(r) = &self.reminders {
            if let Some(out) = r.call(name, input) {
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
            "description": "继续、重试或修改已结束的后台任务，自动带上原任务和完整结果。不填 id 时使用最近任务。用户说接着做、修改刚才结果时优先使用。运行中的任务须先取消。",
            "input_schema": {"type": "object", "properties": {"id": {"type": "integer", "minimum": 1}, "instruction": {"type": "string"}}, "required": ["instruction"]}
        }),
        json!({
            "name": "start_task",
            "description": "把需要多步操作的任务交给后台智能体去做（操作软件或网页、查资料并总结、读写文件、整理东西等），立即返回，不用等它做完。task 要写成完整、独立能看懂的说明，包含用户的原话要点。",
            "input_schema": {"type": "object", "properties": {"task": {"type": "string"}}, "required": ["task"]}
        }),
        json!({
            "name": "task_status",
            "description": "查看后台任务的进度。",
            "input_schema": {"type": "object", "properties": {}}
        }),
        json!({
            "name": "cancel_task",
            "description": "取消后台任务。不给 id 时取消最近一个正在进行的任务。",
            "input_schema": {"type": "object", "properties": {"id": {"type": "integer"}}}
        }),
    ]
}
