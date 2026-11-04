//! 文件操作：按路径直接改名、查找、打开、移动 / 复制、删除（进回收站）、查看信息，不用在资源管理器里一步步点。
//! 没给路径时，用最靠前的资源管理器窗口里选中的文件（用户说“把这个文件……”时就是它）。

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use windows::Win32::UI::Shell::{FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW, SHFileOperationW};
use windows::core::PCWSTR;

use super::ps;
use crate::screen;

/// 读出所有资源管理器窗口的当前文件夹和选中项（Shell.Application 的 COM 接口）
const LIST_EXPLORER: &str = r#"$r=@(); foreach($w in (New-Object -ComObject Shell.Application).Windows()){ try{
 $d=$w.Document; $f=$d.Folder.Self.Path; if(-not $f -or -not (Test-Path -LiteralPath $f)){continue}
 $r+=[pscustomobject]@{hwnd=[int64]$w.HWND; folder=$f; selected=@($d.SelectedItems()|ForEach-Object{$_.Path})}
}catch{} }; ConvertTo-Json -InputObject $r -Compress -Depth 3"#;

/// 先查 Windows 搜索索引（快）；索引不可用或没结果时，在常用文件夹里按名字扫一遍
const FIND: &str = r#"$kw=$env:AERI_KW; $kind=$env:AERI_KIND; $days=[int]$env:AERI_DAYS; $scope=$env:AERI_SCOPE
$rows=@()
try {
  $w=@(); if($kw){ $w+="System.FileName LIKE '%"+$kw.Replace("'","''")+"%'" }
  if($kind){ $w+="System.Kind = '"+$kind+"'" }
  if($days -gt 0){ $w+="System.DateModified >= '"+(Get-Date).AddDays(-$days).ToString('yyyy-MM-dd')+"'" }
  if($scope){ $w+="SCOPE='file:"+$scope.Replace("'","''")+"'" }
  if(-not $w){ $w+="System.DateModified >= '"+(Get-Date).AddDays(-7).ToString('yyyy-MM-dd')+"'" }
  $c=New-Object -ComObject ADODB.Connection; $c.Open("Provider=Search.CollatorDSO;Extended Properties='Application=Windows';")
  $rs=$c.Execute("SELECT TOP 20 System.ItemUrl, System.Size, System.DateModified FROM SystemIndex WHERE "+($w -join ' AND ')+" ORDER BY System.DateModified DESC")
  while(-not $rs.EOF){ $u=[string]$rs.Fields.Item(0).Value; $rows+=[pscustomobject]@{path=($u -replace '^file:','' -replace '/','\'); size=[int64]("0"+$rs.Fields.Item(1).Value); modified=([datetime]$rs.Fields.Item(2).Value).ToLocalTime().ToString('yyyy-MM-dd HH:mm')}; $rs.MoveNext() }
  $c.Close()
} catch {}
if($rows.Count -eq 0 -and $kw){
  $roots = if($scope){ @($scope) } else { @('Desktop','Documents','Downloads','Pictures','Videos','Music') | ForEach-Object { Join-Path $env:USERPROFILE $_ } }
  $since=(Get-Date).AddDays(-[Math]::Max($days,36500))
  $rows=@(foreach($r in $roots){ if(Test-Path -LiteralPath $r){ Get-ChildItem -LiteralPath $r -Recurse -Depth 4 -Force -ErrorAction SilentlyContinue -Filter "*$kw*" } }) |
    Where-Object { $_.LastWriteTime -ge $since } | Sort-Object LastWriteTime -Descending | Select-Object -First 20 |
    ForEach-Object { [pscustomobject]@{path=$_.FullName; size=[int64]($_.Length); modified=$_.LastWriteTime.ToString('yyyy-MM-dd HH:mm')} }
}
ConvertTo-Json -InputObject @($rows) -Compress"#;

#[derive(Deserialize)]
struct ExplorerWindow {
    hwnd: i64,
    folder: String,
    #[serde(default)]
    selected: Vec<String>,
}

#[derive(Deserialize)]
struct Found {
    path: String,
    size: i64,
    modified: String,
}

const PATH_DESC: &str = "完整路径、常用文件夹（桌面、下载、文档、图片、视频、音乐）或当前资源管理器文件夹里的文件名；不填就用资源管理器里选中的";

pub fn definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "file_rename",
            "description": "给文件或文件夹改名，一步完成，不要用资源管理器里按 F2 的方式。用户说“把这个文件改名”时不填 path，自动用资源管理器里选中的那一项。new_name 只写新名字，不带扩展名时保留原来的扩展名。目标名字已存在时不会覆盖。",
            "input_schema": {"type": "object", "properties": {
                "path": {"type": "string", "description": PATH_DESC},
                "new_name": {"type": "string", "description": "新名字，例如“开发程序员”或“开发程序员.pdf”"}
            }, "required": ["new_name"]}
        }),
        json!({
            "name": "file_find",
            "description": "在电脑上查找文件（Windows 搜索索引，一般不到一秒），按修改时间从新到旧最多返回 20 个。用户说“找一下上周的合同”“最近下载的图片”时用。",
            "input_schema": {"type": "object", "properties": {
                "keyword": {"type": "string", "description": "文件名里包含的文字，可不填"},
                "kind": {"type": "string", "enum": ["document", "picture", "video", "music", "folder", "program"], "description": "文件类型，可不填"},
                "days": {"type": "integer", "description": "只找最近几天修改过的，可不填"},
                "folder": {"type": "string", "description": "只在这个文件夹里找（完整路径或桌面、下载等），可不填"}
            }}
        }),
        json!({
            "name": "file_open",
            "description": "用默认程序打开文件，或在资源管理器里打开文件夹（“打开下载文件夹”）。reveal 为 true 时不打开文件，而是在资源管理器里定位并选中它。",
            "input_schema": {"type": "object", "properties": {
                "path": {"type": "string", "description": PATH_DESC},
                "reveal": {"type": "boolean"}
            }}
        }),
        json!({
            "name": "file_move",
            "description": "把文件或文件夹移动（copy 为 true 时复制）到另一个文件夹，不会覆盖同名文件。不填 path 时用资源管理器里选中的（可以多个）。",
            "input_schema": {"type": "object", "properties": {
                "path": {"type": "string", "description": PATH_DESC},
                "to": {"type": "string", "description": "目标文件夹：完整路径或桌面、下载、文档等"},
                "copy": {"type": "boolean"}
            }, "required": ["to"]}
        }),
        json!({
            "name": "file_delete",
            "description": "把文件或文件夹移到回收站（可以从回收站找回）。只有用户明确说要删除时才用。不填 path 时用资源管理器里选中的（可以多个）。",
            "input_schema": {"type": "object", "properties": {"path": {"type": "string", "description": PATH_DESC}}}
        }),
        json!({
            "name": "file_info",
            "description": "查看文件或文件夹的信息：完整路径、大小、类型、修改时间。用户问“这个文件多大”“是什么格式”时用；不填 path 时看资源管理器里选中的（可以多个），什么都没选时返回当前打开的文件夹。",
            "input_schema": {"type": "object", "properties": {"path": {"type": "string", "description": PATH_DESC}}}
        }),
    ]
}

pub async fn call(name: &str, input: &Value) -> Option<(String, bool)> {
    let s = |k: &str| input[k].as_str().unwrap_or("").trim().to_string();
    let result = match name {
        "file_rename" => rename(&s("path"), &s("new_name")).await,
        "file_find" => find(&s("keyword"), &s("kind"), input["days"].as_u64().unwrap_or(0), &s("folder")).await,
        "file_open" => open(&s("path"), input["reveal"].as_bool().unwrap_or(false)).await,
        "file_move" => move_to(&s("path"), &s("to"), input["copy"].as_bool().unwrap_or(false)).await,
        "file_delete" => delete(&s("path")).await,
        "file_info" => info(&s("path")).await,
        _ => return None,
    };
    Some(match result {
        Ok(msg) => (msg, false),
        Err(msg) => (msg, true),
    })
}

// ---------- 改名 ----------

async fn rename(path: &str, new_name: &str) -> Result<String, String> {
    check_name(new_name)?;
    let from = resolve_one(path).await?;
    let to = target(&from, new_name);
    if to == from {
        return Ok(format!("名字没变，已经是「{}」", file_name(&to)));
    }
    // 只改大小写时 Windows 认为目标已存在，其实是同一个文件
    let same_file = to.to_string_lossy().to_lowercase() == from.to_string_lossy().to_lowercase();
    if to.exists() && !same_file {
        return Err(format!("同一文件夹里已经有「{}」，没有改名，请换个名字", file_name(&to)));
    }
    std::fs::rename(&from, &to).map_err(|e| io_error(&from, &e))?;
    Ok(format!("已把「{}」改名为「{}」，位置：{}", file_name(&from), file_name(&to), to.display()))
}

fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("缺少新名字".into());
    }
    if let Some(c) = name.chars().find(|c| r#"\/:*?"<>|"#.contains(*c) || c.is_control()) {
        return Err(format!("新名字里不能有「{c}」"));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err("新名字不能以点或空格结尾".into());
    }
    Ok(())
}

/// 新名字没写扩展名时沿用原来的（用户一般只说“改成开发程序员”）；文件夹不加
fn target(from: &Path, new_name: &str) -> PathBuf {
    let mut name = new_name.to_string();
    if from.is_file() && Path::new(new_name).extension().is_none() {
        if let Some(ext) = from.extension() {
            name = format!("{name}.{}", ext.to_string_lossy());
        }
    }
    from.with_file_name(name)
}

// ---------- 查找 ----------

async fn find(keyword: &str, kind: &str, days: u64, folder: &str) -> Result<String, String> {
    let scope = if folder.is_empty() { String::new() } else { resolve_folder(folder)?.to_string_lossy().into_owned() };
    let env = [
        ("AERI_KW", keyword.replace(['%', '_', '[', ']'], "")),
        ("AERI_KIND", kind.to_string()),
        ("AERI_DAYS", days.to_string()),
        ("AERI_SCOPE", scope),
    ];
    let out = ps::run(FIND, &env, Duration::from_secs(20)).await.map_err(|e| format!("查找失败：{e}"))?;
    let found: Vec<Found> = serde_json::from_str(&out).unwrap_or_default();
    if found.is_empty() {
        return Ok("没有找到符合条件的文件".into());
    }
    let lines: Vec<String> = found.iter().map(|f| format!("{}（{}，{}）", f.path, f.modified, size_text(f.size as u64))).collect();
    Ok(format!("找到 {} 个，从新到旧：\n{}", found.len(), lines.join("\n")))
}

// ---------- 打开 ----------

async fn open(path: &str, reveal: bool) -> Result<String, String> {
    let p = resolve_one(path).await?;
    let ok = if reveal {
        std::process::Command::new("explorer.exe").arg(format!("/select,{}", p.display())).spawn().is_ok()
    } else {
        crate::sys::shell_open(&p.to_string_lossy())
    };
    if !ok {
        return Err(format!("打不开「{}」", file_name(&p)));
    }
    Ok(if reveal { format!("已在资源管理器里选中「{}」", file_name(&p)) } else { format!("已打开「{}」", file_name(&p)) })
}

// ---------- 移动 / 复制 ----------

async fn move_to(path: &str, to: &str, copy: bool) -> Result<String, String> {
    let dest = resolve_folder(to)?;
    if !dest.is_dir() {
        return Err(format!("目标文件夹不存在：{}", dest.display()));
    }
    let items = resolve_many(path).await?;
    let mut done = Vec::new();
    let mut failed = Vec::new();
    for from in &items {
        let target = dest.join(from.file_name().unwrap_or_default());
        if target.exists() {
            failed.push(format!("「{}」目标里已有同名的", file_name(from)));
            continue;
        }
        let r = if copy {
            copy_any(from, &target)
        } else {
            // 跨盘时 rename 不行，先复制再删
            std::fs::rename(from, &target).or_else(|_| copy_any(from, &target).and_then(|_| remove_any(from)))
        };
        match r {
            Ok(()) => done.push(file_name(from)),
            Err(e) => failed.push(io_error(from, &e)),
        }
    }
    let verb = if copy { "复制" } else { "移动" };
    let mut msg = if done.is_empty() { String::new() } else { format!("已把「{}」{verb}到 {}", done.join("」「"), dest.display()) };
    if !failed.is_empty() {
        msg.push_str(&format!("{}没有{verb}：{}", if msg.is_empty() { "" } else { "；" }, failed.join("；")));
    }
    if done.is_empty() { Err(msg) } else { Ok(msg) }
}

fn copy_any(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_any(&e.path(), &to.join(e.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

fn remove_any(p: &Path) -> std::io::Result<()> {
    if p.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) }
}

// ---------- 删除（回收站） ----------

async fn delete(path: &str) -> Result<String, String> {
    let items = resolve_many(path).await?;
    let names: Vec<String> = items.iter().map(|p| file_name(p)).collect();
    tokio::task::spawn_blocking(move || recycle(&items)).await.map_err(|e| e.to_string())??;
    Ok(format!("已把「{}」移到回收站，需要的话可以从回收站还原", names.join("」「")))
}

fn recycle(items: &[PathBuf]) -> Result<(), String> {
    // 多个路径用 \0 分隔，最后再加一个 \0
    let mut buf: Vec<u16> = Vec::new();
    for p in items {
        buf.extend(p.as_os_str().encode_wide_compat());
        buf.push(0);
    }
    buf.push(0);
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(buf.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI).0 as u16,
        ..Default::default()
    };
    let r = unsafe { SHFileOperationW(&mut op) };
    if r != 0 || op.fAnyOperationsAborted.as_bool() {
        return Err(format!("删除失败（错误码 {r}），文件可能正被其他程序使用"));
    }
    Ok(())
}

trait EncodeWide {
    fn encode_wide_compat(&self) -> Vec<u16>;
}
impl EncodeWide for std::ffi::OsStr {
    fn encode_wide_compat(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().collect()
    }
}

// ---------- 信息 ----------

async fn info(path: &str) -> Result<String, String> {
    let items = if path.is_empty() {
        let win = front_explorer().await?;
        if win.selected.is_empty() {
            return Ok(format!("资源管理器当前打开的文件夹：{}，没有选中文件", win.folder));
        }
        win.selected.iter().map(PathBuf::from).collect()
    } else {
        vec![resolve_one(path).await?]
    };
    let lines: Vec<String> = items.iter().take(20).map(|p| describe(p)).collect();
    Ok(lines.join("\n"))
}

fn describe(p: &Path) -> String {
    let Ok(meta) = std::fs::metadata(p) else { return format!("{}（读不到信息）", p.display()) };
    let modified = meta.modified().ok().map(local_time).unwrap_or_default();
    if meta.is_dir() {
        let count = std::fs::read_dir(p).map(|d| d.count()).unwrap_or(0);
        format!("{}：文件夹，里面 {count} 项，修改于 {modified}", p.display())
    } else {
        let ext = p.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_else(|| "无扩展名".into());
        format!("{}：{ext} 文件，{}，修改于 {modified}", p.display(), size_text(meta.len()))
    }
}

fn local_time(t: std::time::SystemTime) -> String {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    let Ok(d) = t.duration_since(std::time::UNIX_EPOCH) else { return String::new() };
    let ticks = d.as_nanos() as u64 / 100 + 116_444_736_000_000_000;
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    unsafe {
        if FileTimeToSystemTime(&ft, &mut utc).is_err() || SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_err() {
            return String::new();
        }
    }
    format!("{}-{:02}-{:02} {:02}:{:02}", local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute)
}

pub fn size_text(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[i]) }
}

// ---------- 找到要操作的文件 ----------

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

fn io_error(p: &Path, e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(32) | Some(33) => format!("「{}」正在被其他程序使用，关掉后再试", file_name(p)),
        Some(5) => format!("没有权限操作「{}」", file_name(p)),
        _ => format!("「{}」操作失败：{e}", file_name(p)),
    }
}

/// 常用文件夹的中文叫法
fn known_folder(name: &str) -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("USERPROFILE")?);
    let sub = match name.trim_end_matches("文件夹").trim_end_matches("目录") {
        "桌面" => "Desktop",
        "下载" => "Downloads",
        "文档" | "我的文档" => "Documents",
        "图片" | "相册" => "Pictures",
        "视频" => "Videos",
        "音乐" => "Music",
        "用户" | "主目录" => return Some(home),
        _ => return None,
    };
    Some(home.join(sub))
}

fn resolve_folder(name: &str) -> Result<PathBuf, String> {
    if let Some(p) = known_folder(name) {
        return Ok(p);
    }
    let p = PathBuf::from(name);
    if p.is_absolute() { Ok(p) } else { Err(format!("「{name}」不是完整路径，请说清楚是哪个文件夹")) }
}

/// 一个文件：完整路径或常用文件夹直接用；只给文件名就在资源管理器当前文件夹里找；都没给用选中的那一项
async fn resolve_one(path: &str) -> Result<PathBuf, String> {
    let mut items = resolve_many(path).await?;
    if items.len() > 1 {
        return Err(format!("资源管理器里选中了 {} 个文件，这个操作一次只能对一个，请说清楚是哪个", items.len()));
    }
    Ok(items.remove(0))
}

async fn resolve_many(path: &str) -> Result<Vec<PathBuf>, String> {
    if let Some(p) = known_folder(path) {
        return Ok(vec![p]);
    }
    let p = PathBuf::from(path);
    if !path.is_empty() && p.is_absolute() {
        return if p.exists() { Ok(vec![p]) } else { Err(format!("找不到「{path}」")) };
    }
    let win = front_explorer().await?;
    if path.is_empty() {
        if win.selected.is_empty() {
            return Err(format!("资源管理器（{}）里没有选中文件，请先选中，或说出文件名", win.folder));
        }
        return Ok(win.selected.iter().map(PathBuf::from).collect());
    }
    let candidate = Path::new(&win.folder).join(path);
    if candidate.exists() {
        return Ok(vec![candidate]);
    }
    // 用户可能没说扩展名：在当前文件夹里找主文件名相同的
    let stem = path.to_lowercase();
    let matches: Vec<PathBuf> = std::fs::read_dir(&win.folder)
        .map_err(|e| format!("读不了文件夹 {}：{e}", win.folder))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_stem().is_some_and(|s| s.to_string_lossy().to_lowercase() == stem))
        .collect();
    match matches.len() {
        1 => Ok(matches),
        0 => Err(format!("在 {} 里找不到「{path}」，可以先用 file_find 查找", win.folder)),
        n => Err(format!("{} 里有 {n} 个叫「{path}」的文件，请带上扩展名", win.folder)),
    }
}

/// 最靠前的资源管理器窗口。Win11 一个窗口有多个标签页时句柄相同，优先选有选中项的那个
async fn front_explorer() -> Result<ExplorerWindow, String> {
    let out = ps::run(LIST_EXPLORER, &[], Duration::from_secs(10)).await.map_err(|e| format!("读取资源管理器失败：{e}"))?;
    let mut wins: Vec<ExplorerWindow> = serde_json::from_str(&out).unwrap_or_default();
    if wins.is_empty() {
        return Err("没有打开的资源管理器窗口，请说出文件的完整路径，或先用 file_find 查找".into());
    }
    let order = screen::window_order();
    let rank = |w: &ExplorerWindow| order.iter().position(|h| *h as i64 == w.hwnd).unwrap_or(usize::MAX);
    wins.sort_by_key(|w| (rank(w), w.selected.is_empty()));
    Ok(wins.into_iter().next().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aeri-files-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn keeps_extension() {
        let dir = temp_dir();
        let f = dir.join("简历.pdf");
        std::fs::write(&f, b"x").unwrap();
        assert_eq!(target(&f, "开发程序员"), dir.join("开发程序员.pdf"));
        assert_eq!(target(&f, "a.txt"), dir.join("a.txt"));
        assert_eq!(target(&dir, "新文件夹"), dir.with_file_name("新文件夹"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_bad_names() {
        assert!(check_name("a/b").is_err());
        assert!(check_name("a.").is_err());
        assert!(check_name("").is_err());
        assert!(check_name("开发程序员").is_ok());
    }

    #[test]
    fn sizes() {
        assert_eq!(size_text(500), "500 B");
        assert_eq!(size_text(189954), "185.5 KB");
    }

    #[tokio::test]
    async fn rename_move_copy_delete_by_path() {
        let dir = temp_dir();
        let sub = dir.join("sub");
        std::fs::create_dir(&sub).unwrap();
        let f = dir.join("old.txt");
        std::fs::write(&f, b"x").unwrap();
        std::fs::write(dir.join("taken.txt"), b"y").unwrap();
        assert!(rename(f.to_str().unwrap(), "taken").await.is_err());
        rename(f.to_str().unwrap(), "new").await.unwrap();
        let f = dir.join("new.txt");
        assert!(f.exists());
        move_to(f.to_str().unwrap(), sub.to_str().unwrap(), true).await.unwrap();
        assert!(f.exists() && sub.join("new.txt").exists());
        assert!(move_to(f.to_str().unwrap(), sub.to_str().unwrap(), false).await.is_err()); // 不覆盖
        assert!(info(f.to_str().unwrap()).await.unwrap().contains("TXT"));
        delete(f.to_str().unwrap()).await.unwrap();
        assert!(!f.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 手动测试：先在资源管理器里选中一个文件，`cargo test reads_explorer -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn reads_explorer() {
        let w = front_explorer().await.unwrap();
        println!("{} {:?}", w.folder, w.selected);
        println!("{}", find("开发", "", 0, "").await.unwrap());
    }
}
