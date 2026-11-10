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
                "instructions": "操作用户 Windows 电脑的工具。桌面软件：先 desktop_snapshot 拿控件编号（窗口不用在前台），然后 desktop_click、desktop_set_value、desktop_select、desktop_check 操作，这些不抢焦点、会确认结果；界面变了要重新 desktop_snapshot。必须模拟键盘时先 desktop_focus 再用 desktop_type / desktop_keys。用户屏幕上正在看的内容用 screen_read 读，复制文字用 clipboard_write 直接写剪贴板。文件操作用 file_ 开头的工具（改名、查找、打开、移动复制、删除进回收站、查看信息；不填路径就用资源管理器里选中的）。系统：volume_set、system_setting、screenshot、power、window_arrange、system_info、calc、clipboard_history。网页：用 browser_ 开头的工具，在独立的 Edge 窗口里打开网页、读页面结构（browser_snapshot）、点击、填写；这个窗口没有用户的登录状态。只是想让用户在自己的浏览器里看某个网站的搜索结果时，用 web_search 一步完成。操作思源、记事本等具体软件前，先用 plugin_guide 读它的插件说明书，照着做（常常是用 local_http 调软件的本机接口，比点界面快得多）。"
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
                reply(&mut stdout, json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("不支持的方法：{other}")}}))?;
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
