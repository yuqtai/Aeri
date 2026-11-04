//! 软件更新：从 GitHub Release 检查新版本，下载、校验后替换安装目录里的文件，再重新启动。
//!
//! 运行中的 exe 不能覆盖自己，所以分两步：
//! 1. 主程序下载 zip、核对 SHA-256、解压到 %TEMP%\aeri-update\new，然后用**新版本里的** aeri.exe
//!    带 `--apply-update` 启动（它旁边就有新版的 DLL），自己退出；
//! 2. 新版本等旧进程退出，把安装目录里要替换的文件先挪到 .update-backup，再复制新文件进去；
//!    任何一步失败就把备份挪回来，保证不会出现装了一半打不开。最后启动安装目录里的新程序。
//! 设置、API Key、提醒都在 %APPDATA%\VoiceAssistant，不在安装目录，更新不会动到。

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, anyhow, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::{self, NoWindow};

pub const REPO: &str = "taiwei18/voice-assistant";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// 自动检查的间隔
const CHECK_EVERY: u64 = 24 * 3600;

#[derive(Clone, Debug)]
pub struct Release {
    /// 不带 v，例如 0.3.1
    pub version: String,
    pub notes: String,
    pub zip_url: String,
    pub sha256: String,
    pub size: u64,
}

static LATEST: std::sync::Mutex<Option<Release>> = std::sync::Mutex::new(None);

/// 最近一次检查到的新版本（界面和语音“更新软件”共用）
pub fn latest() -> Option<Release> {
    LATEST.lock().unwrap().clone()
}

pub fn set_latest(rel: Option<Release>) {
    *LATEST.lock().unwrap() = rel;
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(format!("Aeri-updater/{CURRENT}"))
        .connect_timeout(Duration::from_secs(15))
        .build()
        .unwrap_or_default()
}

/// 有比当前新的版本时返回它；已经是最新返回 None
pub async fn check() -> anyhow::Result<Option<Release>> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let resp = client()
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .context("连不上 GitHub")?;
    if !resp.status().is_success() {
        bail!("GitHub 返回 {}", resp.status());
    }
    let v: Value = resp.json().await.context("GitHub 返回的内容看不懂")?;
    mark_checked();
    let version = v["tag_name"].as_str().unwrap_or("").trim_start_matches('v').to_string();
    if !newer(&version, CURRENT) {
        return Ok(None);
    }
    let assets = v["assets"].as_array().cloned().unwrap_or_default();
    let zip = assets
        .iter()
        .find(|a| a["name"].as_str().is_some_and(|n| n.ends_with("-windows-x64.zip")))
        .ok_or_else(|| anyhow!("v{version} 没有附带安装包"))?;
    let name = zip["name"].as_str().unwrap_or("");
    // GitHub 会给每个附件算 SHA-256（digest 字段）；没有时找同名的 .sha256 文件
    let mut sha256 = zip["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).unwrap_or("").to_lowercase();
    if sha256.is_empty() {
        if let Some(sum) = assets.iter().find(|a| a["name"].as_str() == Some(&format!("{name}.sha256"))) {
            let text = client().get(sum["browser_download_url"].as_str().unwrap_or("")).send().await?.text().await?;
            sha256 = text.split_whitespace().next().unwrap_or("").to_lowercase();
        }
    }
    if sha256.len() != 64 {
        bail!("v{version} 的安装包没有校验值，为了安全不自动更新，请到 GitHub 手动下载");
    }
    Ok(Some(Release {
        version,
        notes: v["body"].as_str().unwrap_or("").to_string(),
        zip_url: zip["browser_download_url"].as_str().unwrap_or("").to_string(),
        sha256,
        size: zip["size"].as_u64().unwrap_or(0),
    }))
}

/// 版本号 a 是否比 b 新（按 x.y.z 数字比较）
pub fn newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> { s.trim_start_matches('v').split(['.', '-']).map_while(|p| p.parse().ok()).collect() };
    let (a, b) = (parse(a), parse(b));
    !a.is_empty() && a > b
}

/// 更新说明的前几行，给界面显示
pub fn notes_summary(notes: &str) -> String {
    let lines: Vec<&str> = notes.lines().map(str::trim).filter(|l| l.starts_with("- ")).take(4).collect();
    lines.iter().map(|l| l.trim_start_matches("- ").replace("**", "")).map(|l| l.chars().take(60).collect::<String>()).collect::<Vec<_>>().join("\n")
}

fn work_dir() -> PathBuf {
    std::env::temp_dir().join("aeri-update")
}

/// 安装目录 = 当前 exe 所在的文件夹；开发时编译出来的程序不更新
pub fn install_dir() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| anyhow!("找不到程序所在的文件夹"))?.to_path_buf();
    let s = dir.to_string_lossy().to_lowercase();
    if s.contains("\\target\\") || s.contains(".cargo-target") {
        bail!("这是开发时编译的程序，不能自动更新");
    }
    Ok(dir)
}

/// 下载并校验安装包，解压到临时目录，返回解压出来的文件夹。progress 是 0-100
pub async fn download(rel: &Release, progress: impl Fn(u8)) -> anyhow::Result<PathBuf> {
    install_dir()?;
    let base = work_dir();
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base)?;
    let zip_path = base.join("update.zip");

    let mut resp = client().get(&rel.zip_url).send().await.context("下载失败")?;
    if !resp.status().is_success() {
        bail!("下载失败：{}", resp.status());
    }
    let total = resp.content_length().unwrap_or(rel.size).max(1);
    let mut file = std::fs::File::create(&zip_path)?;
    let mut hasher = Sha256::new();
    let (mut got, mut last) = (0u64, 0u8);
    loop {
        let chunk = tokio::time::timeout(Duration::from_secs(60), resp.chunk()).await.map_err(|_| anyhow!("下载卡住了，请稍后再试"))??;
        let Some(chunk) = chunk else { break };
        std::io::Write::write_all(&mut file, &chunk)?;
        hasher.update(&chunk);
        got += chunk.len() as u64;
        let p = (got * 100 / total).min(100) as u8;
        if p != last {
            last = p;
            progress(p);
        }
    }
    drop(file);
    let hash = format!("{:x}", hasher.finalize());
    if hash != rel.sha256 {
        let _ = std::fs::remove_file(&zip_path);
        bail!("安装包校验不通过（可能下载不完整或被篡改），已删除，请稍后再试");
    }

    let new = base.join("new");
    std::fs::create_dir_all(&new)?;
    // Windows 10 起自带的 tar 能解 zip
    let out = tokio::process::Command::new("tar.exe").arg("-xf").arg(&zip_path).arg("-C").arg(&new).creation_flags_no_window().output().await?;
    if !out.status.success() {
        bail!("解压失败：{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    for need in ["aeri.exe", "agent-desktop.exe", "models/kws/tokens.txt"] {
        if !new.join(need).exists() {
            bail!("安装包不完整，缺少 {need}");
        }
    }
    Ok(new)
}

/// 用新版本里的 aeri.exe 去替换文件；调用后当前程序应尽快退出
pub fn launch_apply(new: &Path) -> anyhow::Result<()> {
    let install = install_dir()?;
    std::process::Command::new(new.join("aeri.exe"))
        .arg("--apply-update")
        .arg(new)
        .arg(&install)
        .arg(std::process::id().to_string())
        .current_dir(new)
        .creation_flags_no_window()
        .spawn()
        .context("启动更新程序失败")?;
    Ok(())
}

// ---------- 语音：“检查更新”“更新软件” ----------

pub fn definition() -> Value {
    json!({
        "name": "app_update",
        "description": "Aeri 自己的软件更新。check：看 GitHub 上有没有新版本；install：下载新版本并重启完成更新（设置会保留，用户说“更新软件”“升级到最新版”时用）。",
        "input_schema": {"type": "object", "properties": {"action": {"type": "string", "enum": ["check", "install"]}}, "required": ["action"]}
    })
}

pub async fn call(input: &Value) -> (String, bool) {
    let rel = match latest() {
        Some(rel) if input["action"] == "install" => rel,
        _ => match check().await {
            Ok(Some(rel)) => {
                set_latest(Some(rel.clone()));
                if input["action"] != "install" {
                    let notes = notes_summary(&rel.notes).replace('\n', "；");
                    return (format!("有新版本 v{}（当前 v{CURRENT}）。更新内容：{notes}。用户同意后用 action=install 更新", rel.version), false);
                }
                rel
            }
            Ok(None) => return (format!("已经是最新版本 v{CURRENT}"), false),
            Err(e) => return (format!("检查更新失败：{e:#}"), true),
        },
    };
    match download(&rel, |_| {}).await.and_then(|dir| launch_apply(&dir)) {
        Ok(()) => {
            // 留几秒把回复说完再退出，由新版本完成替换并重新启动
            tokio::spawn(async {
                tokio::time::sleep(Duration::from_secs(6)).await;
                let _ = slint::invoke_from_event_loop(|| {
                    let _ = slint::quit_event_loop();
                });
            });
            (format!("v{} 已下载并校验，几秒后 Aeri 会自动重启完成更新", rel.version), false)
        }
        Err(e) => (format!("更新失败：{e:#}"), true),
    }
}

/// `aeri.exe --update`：不开界面直接更新。结果写进 %APPDATA%\VoiceAssistant\update-cli.log
pub fn update_from_command_line() -> anyhow::Result<()> {
    let log = |s: &str| {
        let _ = std::fs::write(config::app_dir().join("update-cli.log"), s);
    };
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let r = rt.block_on(async {
        let Some(rel) = check().await? else { return Ok(format!("已经是最新版本 v{CURRENT}")) };
        let dir = download(&rel, |_| {}).await?;
        launch_apply(&dir)?;
        Ok::<_, anyhow::Error>(format!("正在从 v{CURRENT} 更新到 v{}", rel.version))
    });
    match r {
        Ok(s) => {
            log(&s);
            Ok(())
        }
        Err(e) => {
            log(&format!("更新失败：{e:#}"));
            Err(e)
        }
    }
}

// ---------- 第二步：在新版本里执行 ----------

/// `aeri.exe --apply-update <新文件夹> <安装目录> <旧进程 pid>`
pub fn apply(args: &[String]) -> anyhow::Result<()> {
    let [new, install, pid] = args else { bail!("参数不对") };
    let (new, install) = (PathBuf::from(new), PathBuf::from(install));
    let pid: u32 = pid.parse().unwrap_or(0);
    let from = installed_version(&install);
    let result = (|| {
        wait_exit(pid);
        // agent-desktop 之类从安装目录启动的程序也要先结束，否则文件被占用
        for p in crate::tools::process::pids_in_dir(&install) {
            let _ = std::process::Command::new("taskkill").args(["/F", "/PID", &p.to_string()]).creation_flags_no_window().output();
        }
        std::thread::sleep(Duration::from_millis(500));
        replace_files(&new, &install)
    })();
    let ok = result.is_ok();
    let _ = std::fs::write(
        config::app_dir().join("update-result.json"),
        json!({"ok": ok, "version": CURRENT, "from": from, "error": result.as_ref().err().map(|e| format!("{e:#}"))}).to_string(),
    );
    if ok {
        record_version(&install);
    }
    // 成功启动新的，失败启动原来的（已经恢复）
    let _ = std::process::Command::new(install.join("aeri.exe")).current_dir(&install).spawn();
    result
}

fn wait_exit(pid: u32) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject};
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid) else { return };
        if WaitForSingleObject(h, 20_000).0 != 0 {
            let _ = TerminateProcess(h, 1);
            let _ = WaitForSingleObject(h, 5_000);
        }
        let _ = CloseHandle(h);
    }
}

/// 先把旧文件挪到备份，再复制新文件；出错就全部恢复
pub fn replace_files(new: &Path, install: &Path) -> anyhow::Result<()> {
    let backup = install.join(".update-backup");
    let _ = std::fs::remove_dir_all(&backup);
    std::fs::create_dir_all(&backup)?;
    let names: Vec<std::ffi::OsString> = std::fs::read_dir(new)?.flatten().map(|e| e.file_name()).collect();
    let mut moved = Vec::new();
    let mut added = Vec::new();
    let r = (|| -> anyhow::Result<()> {
        for name in &names {
            let target = install.join(name);
            if target.exists() {
                retry(|| std::fs::rename(&target, backup.join(name))).with_context(|| format!("{} 正在被占用", name.to_string_lossy()))?;
                moved.push(name.clone());
            }
            added.push(name.clone());
            copy_any(&new.join(name), &target).with_context(|| format!("复制 {} 失败", name.to_string_lossy()))?;
        }
        Ok(())
    })();
    if let Err(e) = r {
        for name in &added {
            let p = install.join(name);
            let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
        }
        for name in &moved {
            let _ = std::fs::rename(backup.join(name), install.join(name));
        }
        let _ = std::fs::remove_dir_all(&backup);
        return Err(e.context("更新失败，已恢复原来的版本"));
    }
    let _ = std::fs::remove_dir_all(&backup);
    Ok(())
}

fn retry(mut f: impl FnMut() -> std::io::Result<()>) -> std::io::Result<()> {
    let mut last = None;
    for _ in 0..10 {
        match f() {
            Ok(()) => return Ok(()),
            Err(e) => last = Some(e),
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    Err(last.unwrap())
}

fn copy_any(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_any(&e.path(), &to.join(e.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

fn installed_version(install: &Path) -> String {
    std::fs::read(install.join("installation.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(strip_bom(&b)).ok())
        .and_then(|v| v["version"].as_str().map(str::to_string))
        .unwrap_or_default()
}

fn strip_bom(b: &[u8]) -> &[u8] {
    b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(b)
}

/// 装过的（install.ps1 安装的）同步更新版本记录和“应用和功能”里显示的版本号
fn record_version(install: &Path) {
    let marker = install.join("installation.json");
    if let Some(mut v) = std::fs::read(&marker).ok().and_then(|b| serde_json::from_slice::<Value>(strip_bom(&b)).ok()) {
        v["version"] = json!(CURRENT);
        let _ = std::fs::write(&marker, v.to_string());
        let _ = std::process::Command::new("reg.exe")
            .args(["add", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\VoiceAssistant", "/v", "DisplayVersion", "/t", "REG_SZ", "/d", CURRENT, "/f"])
            .creation_flags_no_window()
            .output();
    }
}

/// 启动时调用：上次更新的结果（给对话框显示一句），顺便清理临时文件
pub fn take_result() -> Option<String> {
    let path = config::app_dir().join("update-result.json");
    let v: Value = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    let _ = std::fs::remove_file(&path);
    // 更新程序可能还没完全退出，稍后再删
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(10));
        let _ = std::fs::remove_dir_all(work_dir());
    });
    Some(if v["ok"].as_bool() == Some(true) {
        format!("已更新到 v{}。", v["version"].as_str().unwrap_or(CURRENT))
    } else {
        format!("更新没有成功：{}。仍在使用原来的版本。", v["error"].as_str().unwrap_or("未知错误"))
    })
}

// ---------- 自动检查的节奏 ----------

fn state_path() -> PathBuf {
    config::app_dir().join("update-state.json")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn mark_checked() {
    let _ = std::fs::write(state_path(), json!({"last_check": now()}).to_string());
}

/// 距上次检查超过一天
pub fn due() -> bool {
    let last = std::fs::read(state_path()).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| v["last_check"].as_u64()).unwrap_or(0);
    now().saturating_sub(last) >= CHECK_EVERY
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(newer("0.3.1", "0.3.0"));
        assert!(newer("v0.10.0", "0.9.9"));
        assert!(newer("1.0.0", "0.99.0"));
        assert!(!newer("0.3.0", "0.3.0"));
        assert!(!newer("0.2.9", "0.3.0"));
        assert!(!newer("", "0.3.0"));
    }

    #[test]
    fn summarizes_notes() {
        let s = notes_summary("开头\n\n## 新功能\n\n- **文件**：改名、查找\n- **截图**：全屏\n\n## 使用\n1. 下载");
        assert_eq!(s, "文件：改名、查找\n截图：全屏");
    }

    #[test]
    fn replaces_and_rolls_back() {
        let root = std::env::temp_dir().join(format!("aeri-update-test-{}", uuid::Uuid::new_v4()));
        let (new, install) = (root.join("new"), root.join("install"));
        std::fs::create_dir_all(new.join("models")).unwrap();
        std::fs::create_dir_all(install.join("models")).unwrap();
        std::fs::write(new.join("aeri.exe"), "new").unwrap();
        std::fs::write(new.join("models/m.txt"), "new").unwrap();
        std::fs::write(install.join("aeri.exe"), "old").unwrap();
        std::fs::write(install.join("models/m.txt"), "old").unwrap();
        std::fs::write(install.join("installation.json"), "{}").unwrap();
        replace_files(&new, &install).unwrap();
        assert_eq!(std::fs::read_to_string(install.join("aeri.exe")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(install.join("models/m.txt")).unwrap(), "new");
        assert!(install.join("installation.json").exists() && !install.join(".update-backup").exists());

        // 新文件里有一个复制不了的（被独占打开），要恢复成原样
        std::fs::write(install.join("aeri.exe"), "old").unwrap();
        std::fs::write(new.join("z-locked.dll"), "new").unwrap();
        std::fs::write(install.join("z-locked.dll"), "old").unwrap();
        use std::os::windows::fs::OpenOptionsExt;
        let lock = std::fs::OpenOptions::new().read(true).share_mode(0).open(install.join("z-locked.dll")).unwrap();
        assert!(replace_files(&new, &install).is_err());
        drop(lock);
        assert_eq!(std::fs::read_to_string(install.join("aeri.exe")).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(install.join("z-locked.dll")).unwrap(), "old");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// 手动测试：`cargo test checks_github -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn checks_github() {
        println!("{:?}", check().await);
    }
}
