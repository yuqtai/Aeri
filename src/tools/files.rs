//! 文件操作：按路径直接改名、查找、打开、移动 / 复制、删除（进回收站）、查看信息，不用在资源管理器里一步步点。
//! 没给路径时，用最靠前的资源管理器窗口里选中的文件（用户说“把这个文件……”时就是它）。

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music, FOLDERID_Pictures, FOLDERID_Videos,
    FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, KF_FLAG_DEFAULT, SHFILEOPSTRUCTW,
    SHFileOperationW, SHGetKnownFolderPath,
};
use windows::Win32::UI::WindowsAndMessaging::GetWindowTextW;
use windows::core::{GUID, PCWSTR};

use super::ps;
use crate::screen;

fn workspace_denied() -> String {
    crate::locale::pick(
        "Current permission is set to \"workspace only\"; modifying or deleting files outside the workspace is not allowed",
        "当前权限设为“只改工作目录”，不允许修改或删除工作目录以外的文件",
    )
    .into()
}

fn no_front_selection() -> String {
    crate::locale::pick(
        "File Explorer is not in the foreground and no desktop file is selected; switch to the folder and select a file first, or specify the file name",
        "当前前台不是资源管理器且桌面没有选中文件，请先切到对应文件夹选中文件，或直接说出文件名",
    )
    .into()
}

/// 读出所有资源管理器窗口（含标签页显示名）以及桌面的选中项（Shell.Application 的 COM 接口）
const LIST_EXPLORER: &str = r#"$r=@(); $sh=New-Object -ComObject Shell.Application
foreach($w in $sh.Windows()){ try{
 $d=$w.Document; $f=$d.Folder.Self.Path; if(-not $f -or -not (Test-Path -LiteralPath $f)){continue}
 $sel=@($d.SelectedItems()|ForEach-Object{$_.Path}|Where-Object{$_ -and (Test-Path -LiteralPath $_)})
 $r+=[pscustomobject]@{hwnd=[int64]$w.HWND; folder=[string]$f; location=[string]$w.LocationName; is_desktop=$false; selected=$sel}
}catch{} }
try {
 [int]$dh=0; $dw=$sh.Windows().FindWindowSW(0, 0, 8, [ref]$dh, 1)
 if($dw){
  $dd=$dw.Document; $df=$dd.Folder.Self.Path
  if(-not $df){ $df=[Environment]::GetFolderPath('Desktop') }
  $dsel=@($dd.SelectedItems()|ForEach-Object{$_.Path}|Where-Object{$_ -and (Test-Path -LiteralPath $_)})
  $r+=[pscustomobject]@{hwnd=[int64]$dh; folder=[string]$df; location='Desktop'; is_desktop=$true; selected=$dsel}
 }
}catch{}
if($r.Count -eq 0){ '[]' } else { ConvertTo-Json -InputObject @($r) -Compress -Depth 3 }"#;

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
  $roots = if($scope){ @($scope) } elseif($env:AERI_ROOTS){ @($env:AERI_ROOTS -split ';' | Where-Object { $_ }) } else { @('Desktop','Documents','Downloads','Pictures','Videos','Music') | ForEach-Object { Join-Path $env:USERPROFILE $_ } }
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
    location: String,
    #[serde(default)]
    is_desktop: bool,
    #[serde(default)]
    selected: Vec<String>,
}

#[derive(Deserialize)]
struct Found {
    path: String,
    size: i64,
    modified: String,
}

const PATH_DESC: &str = "Full path, common folder (Desktop, Downloads, Documents, Pictures, Videos, Music), or a file name in the current Explorer folder; omit for selected Explorer items.";

pub fn definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "file_rename",
            "description": "Rename a file or folder directly; do not press F2 in Explorer. Omit path for the item selected in Explorer when the user says this file. new_name is only the new name; omitting an extension preserves the existing one. Never overwrites an existing target.",
            "input_schema": {"type": "object", "properties": {
                "path": {"type": "string", "description": PATH_DESC},
                "new_name": {"type": "string", "description": "New name, for example Report or Report.pdf."}
            }, "required": ["new_name"]}
        }),
        json!({
            "name": "file_find",
            "description": "Find files using the Windows search index, usually under a second. Returns at most 20, newest modified first. Use for requests such as last week's contract or recently downloaded pictures.",
            "input_schema": {"type": "object", "properties": {
                "keyword": {"type": "string", "description": "Optional text contained in the file name."},
                "kind": {"type": "string", "enum": ["document", "picture", "video", "music", "folder", "program"], "description": "Optional file type."},
                "days": {"type": "integer", "description": "Optional number of recent days in which the file was modified."},
                "folder": {"type": "string", "description": "Optional folder to search: full path or a common folder such as Desktop or Downloads."}
            }}
        }),
        json!({
            "name": "file_open",
            "description": "Open a file in its default application, or a folder in Explorer (for example Downloads). reveal=true locates and selects the file in Explorer instead of opening it.",
            "input_schema": {"type": "object", "properties": {
                "path": {"type": "string", "description": PATH_DESC},
                "reveal": {"type": "boolean"}
            }}
        }),
        json!({
            "name": "file_move",
            "description": "Move a file or folder (copy=true copies) into another folder without overwriting existing names. Omit path to use selected Explorer items, possibly multiple.",
            "input_schema": {"type": "object", "properties": {
                "path": {"type": "string", "description": PATH_DESC},
                "to": {"type": "string", "description": "Destination folder: full path or Desktop, Downloads, Documents, etc."},
                "copy": {"type": "boolean"}
            }, "required": ["to"]}
        }),
        json!({
            "name": "file_delete",
            "description": "Move files or folders to the Recycle Bin, where they can be restored. Only use when the user explicitly requests deletion. Omit path for selected Explorer items, possibly multiple.",
            "input_schema": {"type": "object", "properties": {"path": {"type": "string", "description": PATH_DESC}}}
        }),
        json!({
            "name": "file_info",
            "description": "Get full path, size, type and modification time for files/folders. Use for questions about size or format. Omit path for Explorer selections (possibly multiple); with no selection returns the currently open folder.",
            "input_schema": {"type": "object", "properties": {"path": {"type": "string", "description": PATH_DESC}}}
        }),
    ]
}

pub async fn call_with_limit(name: &str, input: &Value, workspace_limit: Option<&Path>) -> Option<(String, bool)> {
    let s = |k: &str| input[k].as_str().unwrap_or("").trim().to_string();
    let result = match name {
        "file_rename" => rename(&s("path"), &s("new_name"), workspace_limit).await,
        "file_find" => find(&s("keyword"), &s("kind"), input["days"].as_u64().unwrap_or(0), &s("folder")).await,
        "file_open" => open(&s("path"), input["reveal"].as_bool().unwrap_or(false)).await,
        "file_move" => move_to(&s("path"), &s("to"), input["copy"].as_bool().unwrap_or(false), workspace_limit).await,
        "file_delete" => delete(&s("path"), workspace_limit).await,
        "file_info" => info(&s("path")).await,
        _ => return None,
    };
    Some(match result {
        Ok(msg) => (msg, false),
        Err(msg) => (msg, true),
    })
}

// ---------- 改名 ----------

async fn rename(path: &str, new_name: &str, workspace_limit: Option<&Path>) -> Result<String, String> {
    check_name(new_name)?;
    let from = resolve_one(path).await?;
    check_workspace(&from, workspace_limit, false)?;
    let to = target(&from, new_name);
    check_workspace(&to, workspace_limit, false)?;
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

/// 判断 new_name 是否已自带合法扩展名：与原扩展名一致（忽略大小写），
/// 或后缀为 1~8 位全 ASCII 字母数字且至少含一个英文字母（如 .pdf/.mp4/.docx/.7z）。
/// 像「周报v1.2」「11.18纪要」中的 .2 / .18纪要 不是扩展名，应保留原扩展名。
fn has_explicit_extension(new_name: &str, orig_ext: &str) -> bool {
    let Some(ext) = Path::new(new_name).extension().and_then(|e| e.to_str()) else {
        return false;
    };
    if ext.eq_ignore_ascii_case(orig_ext) {
        return true;
    }
    (1..=8).contains(&ext.len())
        && ext.bytes().all(|b| b.is_ascii_alphanumeric())
        && ext.bytes().any(|b| b.is_ascii_alphabetic())
}

/// 新名字没写扩展名时沿用原来的（用户一般只说“改成开发程序员”）；文件夹不加
fn target(from: &Path, new_name: &str) -> PathBuf {
    let mut name = new_name.to_string();
    if from.is_file() {
        if let Some(orig_ext) = from.extension().and_then(|e| e.to_str()).filter(|e| !e.is_empty()) {
            if !has_explicit_extension(new_name, orig_ext) {
                name = format!("{name}.{orig_ext}");
            }
        }
    }
    from.with_file_name(name)
}

// ---------- 查找 ----------

async fn find(keyword: &str, kind: &str, days: u64, folder: &str) -> Result<String, String> {
    let scope = if folder.is_empty() { String::new() } else { resolve_folder(folder)?.to_string_lossy().into_owned() };
    let roots = default_search_roots().into_iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>().join(";");
    let env = [
        ("AERI_KW", keyword.replace(['%', '_', '[', ']'], "")),
        ("AERI_KIND", kind.to_string()),
        ("AERI_DAYS", days.to_string()),
        ("AERI_SCOPE", scope),
        ("AERI_ROOTS", roots),
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

async fn move_to(path: &str, to: &str, copy: bool, workspace_limit: Option<&Path>) -> Result<String, String> {
    let dest = resolve_folder(to)?;
    check_workspace(&dest, workspace_limit, true)?;
    if !dest.is_dir() {
        return Err(format!("目标文件夹不存在：{}", dest.display()));
    }
    let items = resolve_many(path).await?;
    for from in &items {
        check_workspace(from, workspace_limit, false)?;
        let target = dest.join(from.file_name().unwrap_or_default());
        check_workspace(&target, workspace_limit, false)?;
    }
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

async fn delete(path: &str, workspace_limit: Option<&Path>) -> Result<String, String> {
    let items = resolve_many(path).await?;
    for p in &items {
        check_workspace(p, workspace_limit, false)?;
    }
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
        let win = front_explorer(true).await?;
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

/// 解析 Windows 已知系统文件夹（支持 OneDrive 重定向桌面/文档/图片等）；失败时回退 USERPROFILE\fallback_sub
pub(crate) fn known_folder_path(id: &GUID, fallback_sub: &str) -> PathBuf {
    unsafe {
        if let Ok(pwstr) = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) {
            let s = pwstr.to_string().unwrap_or_default();
            CoTaskMemFree(Some(pwstr.0 as *const _));
            if !s.is_empty() {
                return PathBuf::from(s);
            }
        }
    }
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(fallback_sub)
}

fn default_search_roots() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for (id, sub) in [
        (&FOLDERID_Desktop, "Desktop"),
        (&FOLDERID_Documents, "Documents"),
        (&FOLDERID_Downloads, "Downloads"),
        (&FOLDERID_Pictures, "Pictures"),
        (&FOLDERID_Videos, "Videos"),
        (&FOLDERID_Music, "Music"),
    ] {
        let p = known_folder_path(id, sub);
        if !out.iter().any(|existing: &PathBuf| existing.to_string_lossy().eq_ignore_ascii_case(&p.to_string_lossy())) {
            out.push(p);
        }
    }
    out
}

/// 常用文件夹的中英文叫法
fn known_folder(name: &str) -> Option<PathBuf> {
    let trimmed = name.trim().trim_end_matches("文件夹").trim_end_matches("目录").trim();
    match trimmed.to_ascii_lowercase().as_str() {
        "桌面" | "desktop" => Some(known_folder_path(&FOLDERID_Desktop, "Desktop")),
        "下载" | "downloads" | "download" => Some(known_folder_path(&FOLDERID_Downloads, "Downloads")),
        "文档" | "我的文档" | "documents" | "my documents" => Some(known_folder_path(&FOLDERID_Documents, "Documents")),
        "图片" | "相册" | "pictures" | "photos" => Some(known_folder_path(&FOLDERID_Pictures, "Pictures")),
        "视频" | "videos" => Some(known_folder_path(&FOLDERID_Videos, "Videos")),
        "音乐" | "music" => Some(known_folder_path(&FOLDERID_Music, "Music")),
        "用户" | "主目录" | "home" => std::env::var_os("USERPROFILE").map(PathBuf::from),
        _ => None,
    }
}

fn canonical_or_lexical(p: &Path) -> PathBuf {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    };
    let mut clean = PathBuf::new();
    for comp in abs.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if matches!(clean.components().next_back(), Some(std::path::Component::Normal(_))) {
                    clean.pop();
                }
            }
            other => clean.push(other.as_os_str()),
        }
    }
    let mut tail = Vec::new();
    let mut cur: &Path = &clean;
    while !cur.exists() {
        if let Some(name) = cur.file_name() {
            tail.push(name.to_os_string());
            if let Some(parent) = cur.parent() {
                cur = parent;
                continue;
            }
        }
        break;
    }
    let mut base = std::fs::canonicalize(cur).unwrap_or_else(|_| cur.to_path_buf());
    for part in tail.into_iter().rev() {
        base.push(part);
    }
    let s = base.to_string_lossy();
    if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{unc}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        base
    }
}

pub(crate) fn is_within_workspace(path: &Path, workspace: &Path, allow_root: bool) -> bool {
    let p = canonical_or_lexical(path);
    let ws = canonical_or_lexical(workspace);
    let mut p_comps = p.components();
    for ws_comp in ws.components() {
        let Some(p_comp) = p_comps.next() else {
            return false;
        };
        let a = p_comp.as_os_str().to_string_lossy();
        let b = ws_comp.as_os_str().to_string_lossy();
        if !a.eq_ignore_ascii_case(&b) && a.to_lowercase() != b.to_lowercase() {
            return false;
        }
    }
    allow_root || p_comps.next().is_some()
}

fn check_workspace(path: &Path, limit: Option<&Path>, allow_root: bool) -> Result<(), String> {
    let Some(ws) = limit else { return Ok(()) };
    if !is_within_workspace(path, ws, allow_root) {
        return Err(workspace_denied());
    }
    Ok(())
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
    let win = front_explorer(path.is_empty()).await?;
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

fn top_window_title(hwnd: i64) -> String {
    let h = HWND(hwnd as isize as *mut _);
    if h.is_invalid() {
        return String::new();
    }
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(h, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize]).replace('\u{200b}', "").trim().to_string()
}

/// Win11 同一个资源管理器窗口有多个标签页时 HWND 相同：按顶层窗口标题匹配激活标签页的 LocationName / 文件夹名
fn tab_title_score(win_title: &str, location: &str, folder: &str) -> (u8, std::cmp::Reverse<usize>) {
    let wt = win_title.replace('\u{200b}', "").trim().to_lowercase();
    if wt.is_empty() {
        return (2, std::cmp::Reverse(0));
    }
    let leaf = Path::new(folder).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut best = (2u8, std::cmp::Reverse(0usize));
    for cand in [location.trim(), leaf.trim(), folder.trim()] {
        if cand.is_empty() {
            continue;
        }
        let c = cand.to_lowercase();
        let exact_sep = wt == c
            || wt.strip_prefix(&c).is_some_and(|rest| {
                rest.starts_with(" - ")
                    || rest.starts_with(" 和")
                    || rest.starts_with(" and ")
                    || rest.starts_with('\t')
            });
        let cur = if exact_sep {
            (0, std::cmp::Reverse(c.len()))
        } else if wt.starts_with(&c) {
            (1, std::cmp::Reverse(c.len()))
        } else {
            continue;
        };
        if cur < best {
            best = cur;
        }
    }
    best
}

/// 获取用户正在操作的资源管理器标签页或桌面。
/// - `require_front_selection = true`（未传 path，“把这个文件……”）：
///   若前台是桌面且桌面有选中项，返回桌面；否则只允许取 `window_order` 里 `rank == 0` 的前台资源管理器，
///   绝不触碰后台（`rank > 0`）或已最小化（`rank == usize::MAX`）窗口里的旧选中项！
async fn front_explorer(require_front_selection: bool) -> Result<ExplorerWindow, String> {
    let out = ps::run(LIST_EXPLORER, &[], Duration::from_secs(10)).await.map_err(|e| format!("读取资源管理器失败：{e}"))?;
    let all: Vec<ExplorerWindow> = serde_json::from_str(&out).unwrap_or_default();
    let mut desktop: Option<ExplorerWindow> = None;
    let mut wins: Vec<ExplorerWindow> = Vec::new();
    for w in all {
        if w.is_desktop {
            desktop = Some(w);
        } else {
            wins.push(w);
        }
    }

    let desk_front = screen::desktop_is_front();
    let order = screen::window_order();

    if require_front_selection {
        if desk_front {
            if let Some(d) = desktop.filter(|d| !d.selected.is_empty()) {
                return Ok(d);
            }
            return Err(no_front_selection());
        }
        if order.is_empty() {
            if let Some(d) = desktop.filter(|d| !d.selected.is_empty()) {
                return Ok(d);
            }
            return Err(no_front_selection());
        }
        let front_hwnd = order[0] as i64;
        let mut front_tabs: Vec<ExplorerWindow> = wins.into_iter().filter(|w| w.hwnd == front_hwnd).collect();
        if front_tabs.is_empty() {
            return Err(no_front_selection());
        }
        let title = top_window_title(front_hwnd);
        front_tabs.sort_by_key(|w| (tab_title_score(&title, &w.location, &w.folder), w.selected.is_empty()));
        return Ok(front_tabs.into_iter().next().unwrap());
    }

    // 传了相对文件名时：桌面在前台则先看桌面，否则看最前面的可见资源管理器激活标签页
    if desk_front {
        if let Some(d) = desktop {
            return Ok(d);
        }
    }
    let rank = |w: &ExplorerWindow| order.iter().position(|h| *h as i64 == w.hwnd).unwrap_or(usize::MAX);
    wins.retain(|w| rank(w) < usize::MAX);
    if wins.is_empty() {
        if order.is_empty() {
            if let Some(d) = desktop {
                return Ok(d);
            }
        }
        return Err(crate::locale::pick(
            "No File Explorer window is open; specify the full file path or search with file_find first",
            "没有打开的资源管理器窗口，请说出文件的完整路径，或先用 file_find 查找",
        )
        .into());
    }
    let top_hwnd = wins.iter().min_by_key(|w| rank(w)).map(|w| w.hwnd).unwrap_or(0);
    let title = top_window_title(top_hwnd);
    wins.sort_by_key(|w| {
        let r = rank(w);
        let tab = if w.hwnd == top_hwnd { tab_title_score(&title, &w.location, &w.folder) } else { (2, std::cmp::Reverse(0)) };
        (r, tab, w.selected.is_empty())
    });
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
        assert_eq!(target(&f, "开发程序员.pdf"), dir.join("开发程序员.pdf"));
        assert_eq!(target(&f, "开发程序员.PDF"), dir.join("开发程序员.PDF"));
        assert_eq!(target(&f, "a.txt"), dir.join("a.txt"));
        assert_eq!(target(&f, "归档.7z"), dir.join("归档.7z"));

        let doc = dir.join("旧周报.docx");
        std::fs::write(&doc, b"x").unwrap();
        assert_eq!(target(&doc, "周报v1.2"), dir.join("周报v1.2.docx"));
        assert_eq!(target(&doc, "11.18纪要"), dir.join("11.18纪要.docx"));
        assert_eq!(target(&doc, "周报v1.2.docx"), dir.join("周报v1.2.docx"));

        assert_eq!(target(&dir, "新文件夹"), dir.with_file_name("新文件夹"));
        assert_eq!(target(&dir, "项目v1.2"), dir.with_file_name("项目v1.2"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn matches_win11_active_tab_title() {
        let active = tab_title_score("下载 和另外 1 个选项卡 - 文件资源管理器", "下载", r"C:\Users\Test\Downloads");
        let inactive = tab_title_score("下载 和另外 1 个选项卡 - 文件资源管理器", "文档", r"C:\Users\Test\Documents");
        assert!(active < inactive);

        let en_active = tab_title_score("Downloads and 1 more tab - File Explorer", "Downloads", r"C:\Users\Test\Downloads");
        let en_inactive = tab_title_score("Downloads and 1 more tab - File Explorer", "Documents", r"C:\Users\Test\Documents");
        assert!(en_active < en_inactive);
    }

    #[test]
    fn rejects_bad_names() {
        assert!(check_name("a/b").is_err());
        assert!(check_name("a.").is_err());
        assert!(check_name("").is_err());
        assert!(check_name("开发程序员").is_ok());
        assert!(known_folder("桌面").is_some());
        assert!(known_folder("Downloads").is_some());
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
        assert!(rename(f.to_str().unwrap(), "taken", None).await.is_err());
        rename(f.to_str().unwrap(), "new", None).await.unwrap();
        let f = dir.join("new.txt");
        assert!(f.exists());
        move_to(f.to_str().unwrap(), sub.to_str().unwrap(), true, None).await.unwrap();
        assert!(f.exists() && sub.join("new.txt").exists());
        assert!(move_to(f.to_str().unwrap(), sub.to_str().unwrap(), false, None).await.is_err()); // 不覆盖
        assert!(info(f.to_str().unwrap()).await.unwrap().contains("TXT"));
        delete(f.to_str().unwrap(), None).await.unwrap();
        assert!(!f.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn enforces_workspace_boundary() {
        let root = temp_dir();
        let ws = root.join("work");
        let outside = root.join("outside");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let inside_file = ws.join("a.txt");
        let outside_file = outside.join("b.txt");
        std::fs::write(&inside_file, b"1").unwrap();
        std::fs::write(&outside_file, b"2").unwrap();

        assert_eq!(rename(outside_file.to_str().unwrap(), "c", Some(&ws)).await.unwrap_err(), workspace_denied());
        assert_eq!(delete(outside_file.to_str().unwrap(), Some(&ws)).await.unwrap_err(), workspace_denied());
        assert_eq!(move_to(inside_file.to_str().unwrap(), outside.to_str().unwrap(), true, Some(&ws)).await.unwrap_err(), workspace_denied());
        assert_eq!(move_to(outside_file.to_str().unwrap(), ws.to_str().unwrap(), true, Some(&ws)).await.unwrap_err(), workspace_denied());

        rename(inside_file.to_str().unwrap(), "renamed", Some(&ws)).await.unwrap();
        assert!(ws.join("renamed.txt").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// 手动测试：先在资源管理器里选中一个文件，`cargo test reads_explorer -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn reads_explorer() {
        let w = front_explorer(true).await.unwrap();
        println!("{} {:?}", w.folder, w.selected);
        println!("{}", find("开发", "", 0, "").await.unwrap());
    }
}
