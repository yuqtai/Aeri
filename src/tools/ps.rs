//! 少数系统功能（Windows 搜索索引、无线开关、亮度、截图）只有 COM / WinRT / WMI 接口，用 PowerShell 调最省事。
//! 参数一律通过环境变量 AERI_* 传进去，不拼进脚本，避免文件名里的引号、$ 之类被当成代码。

use std::time::Duration;

use crate::config::NoWindow;

pub async fn run(script: &str, env: &[(&str, String)], timeout: Duration) -> Result<String, String> {
    let script = format!("$ErrorActionPreference='Stop'\n[Console]::OutputEncoding=[Text.Encoding]::UTF8\n{script}");
    let mut cmd = tokio::process::Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script]).creation_flags_no_window();
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.kill_on_drop(true);
    let out = tokio::time::timeout(timeout, cmd.output())
        .await
        .map_err(|_| "执行超时".to_string())?
        .map_err(|e| format!("启动 PowerShell 失败：{e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() {
        Ok(stdout)
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let first = err.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("未知错误");
        Err(first.chars().take(200).collect())
    }
}
