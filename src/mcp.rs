//! `aeri.exe --mcp`：把本程序的工具（启动/关闭程序、通过 agent-desktop 读控件、点击、填写、勾选、快捷键、读屏，
//! 以及在独立 Edge 窗口里打开网页、点击、填写、读取页面的浏览器工具）以 MCP（标准输入输出 + JSON-RPC）
//! 提供给 Codex / Claude Code 这些后台智能体。设置里关掉的桌面/浏览器控制，这里也不提供。
//! 这个模式不开界面、不占麦克风，由智能体按需启动，任务结束时随它一起退出。

use std::io::{BufRead, Write};
use std::sync::Arc;

use serde_json::{Value, json};

use crate::tools::Tools;
use crate::tools::apps::AppIndex;

pub fn serve() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let _rt = runtime.enter(); // 浏览器控制在创建时会起后台任务
    let apps = Arc::new(AppIndex::load_cached());
    let tools = Tools::for_agents(&crate::config::load(), apps);
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(id) = req.get("id").cloned() else { continue }; // 通知不用回复
        let result = match req["method"].as_str().unwrap_or("") {
            "initialize" => json!({
                "protocolVersion": req["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "voice_desktop", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "Tools for the user's Windows computer. Reply in the language the user speaks. Desktop apps: get references with desktop_snapshot (no foreground focus needed), then use desktop_click, desktop_set_value, desktop_select, desktop_check, which do not steal focus and verify results; refresh the snapshot after UI changes. For simulated keyboard input, desktop_focus first, then desktop_type / desktop_keys. Read what the user is viewing with screen_read; write copied text directly with clipboard_write. Use file_ tools for rename, find, open, move/copy, Recycle Bin deletion and information; omit paths to use Explorer selections. System tools: volume_set, system_setting, screenshot, power, window_arrange, system_info, calc, clipboard_history. For webpages use browser_ tools in a separate Edge window: navigate, browser_snapshot, click, fill; this window has no user login session. For search results the user wants to view in their own browser, use web_search in one step. Before operating specific software (SiYuan, Notepad, etc.), first read plugin_guide and follow it, often using local_http for its local API rather than clicking the UI."
            }),
            "ping" => json!({}),
            "tools/list" => json!({"tools": tools.definitions().iter().map(to_mcp_tool).collect::<Vec<_>>()}),
            "tools/call" => {
                let name = req["params"]["name"].as_str().unwrap_or("");
                let args = req["params"].get("arguments").cloned().unwrap_or_else(|| json!({}));
                let (text, is_error) = runtime.block_on(tools.call(name, &args));
                json!({"content": [{"type": "text", "text": text}], "isError": is_error})
            }
            other => {
                reply(&mut stdout, json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("Unsupported method: {other}")}}))?;
                continue;
            }
        };
        reply(&mut stdout, json!({"jsonrpc": "2.0", "id": id, "result": result}))?;
    }
    Ok(())
}

fn reply(out: &mut std::io::Stdout, msg: Value) -> anyhow::Result<()> {
    writeln!(out, "{msg}")?;
    out.flush()?;
    Ok(())
}

/// 我们的工具定义是 Anthropic 格式（input_schema），MCP 用 inputSchema
fn to_mcp_tool(def: &Value) -> Value {
    json!({"name": def["name"], "description": def["description"], "inputSchema": def["input_schema"]})
}
