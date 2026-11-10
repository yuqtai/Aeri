//! 系统类工具：精确音量、系统开关（亮度、深色模式、Wi-Fi、蓝牙…）、截图、电源、窗口排列、系统状态、计算器、剪贴板历史。

use std::collections::VecDeque;
use std::sync::{Mutex, Once};
use std::time::Duration;

use serde_json::{Value, json};
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator, eConsole, eRender};
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetLogicalDrives};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::Shutdown::LockWorkStation;
use windows::Win32::System::SystemInformation::{GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_D, VK_LWIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    IsZoomed, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos, ShowWindow,
};
use windows::core::{BOOL, HSTRING};

use super::ps;
use crate::screen;

const CF_UNICODETEXT: u32 = 13;
/// 剪贴板历史保留几条
const CLIP_KEEP: usize = 20;

pub fn definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "volume_set",
            "description": "Set system volume to a percentage (for example, volume to 30), or mute/unmute. Use media_control for small increases or decreases.",
            "input_schema": {"type": "object", "properties": {
                "percent": {"type": "integer", "minimum": 0, "maximum": 100},
                "mute": {"type": "boolean", "description": "true to mute, false to unmute."}
            }}
        }),
        json!({
            "name": "system_setting",
            "description": "Change system settings: brightness uses value 0-100 (built-in displays such as laptops only); dark_mode, wifi and bluetooth use on true/false. Windows prevents direct changes to night_light, do_not_disturb, display and sound: these open the corresponding settings page for the user to change.",
            "input_schema": {"type": "object", "properties": {
                "setting": {"type": "string", "enum": ["brightness", "dark_mode", "wifi", "bluetooth", "night_light", "do_not_disturb", "display", "sound"]},
                "on": {"type": "boolean"},
                "value": {"type": "integer", "minimum": 0, "maximum": 100}
            }, "required": ["setting"]}
        }),
        json!({
            "name": "screenshot",
            "description": "Save a screenshot to Pictures\\Screenshots and copy it to the clipboard for pasting. target=window captures only the window the user is viewing; default captures all screens.",
            "input_schema": {"type": "object", "properties": {"target": {"type": "string", "enum": ["screen", "window"]}}}
        }),
        json!({
            "name": "power",
            "description": "Power actions: lock, sleep, shutdown, restart, cancel (cancel shutdown/restart). Shutdown and restart close all programs: first ask the user to confirm, then call with confirmed=true only after confirmation. There are 30 seconds to cancel after scheduling.",
            "input_schema": {"type": "object", "properties": {
                "action": {"type": "string", "enum": ["lock", "sleep", "shutdown", "restart", "cancel"]},
                "confirmed": {"type": "boolean"}
            }, "required": ["action"]}
        }),
        json!({
            "name": "window_arrange",
            "description": "Arrange a window: maximize, minimize, restore, left/right half of screen, next_monitor, show_desktop (minimize all). window is part of its title; omit for the window the user is viewing.",
            "input_schema": {"type": "object", "properties": {
                "action": {"type": "string", "enum": ["maximize", "minimize", "restore", "left", "right", "next_monitor", "show_desktop"]},
                "window": {"type": "string"}
            }, "required": ["action"]}
        }),
        json!({
            "name": "system_info",
            "description": "Inspect computer status. overview: battery, memory, free disk space and uptime; cpu: CPU usage and top resource-consuming processes; network: connections, Wi-Fi name and signal.",
            "input_schema": {"type": "object", "properties": {"topic": {"type": "string", "enum": ["overview", "cpu", "network"]}}}
        }),
        json!({
            "name": "calc",
            "description": "Calculate mathematical expressions accurately. Always use this for arithmetic instead of mental calculation. Supports + - * / % ^, parentheses, sqrt abs round floor ceil ln log sin cos tan (degrees), pi and e. Express unit conversions as calculations, for example 5*1.609.",
            "input_schema": {"type": "object", "properties": {"expression": {"type": "string"}}, "required": ["expression"]}
        }),
        json!({
            "name": "clipboard_history",
            "description": "Read recently copied text (recorded since Aeri started, up to 20 entries, newest first). Set restore to an entry number to put it back on the clipboard so the user can paste a previous copy.",
            "input_schema": {"type": "object", "properties": {"restore": {"type": "integer", "minimum": 1}}}
        }),
    ]
}

pub async fn call(name: &str, input: &Value) -> Option<(String, bool)> {
    let s = |k: &str| input[k].as_str().unwrap_or("").trim().to_string();
    let result = match name {
        "volume_set" => {
            let (p, m) = (input["percent"].as_u64().map(|v| v.min(100) as u32), input["mute"].as_bool());
            blocking(move || set_volume(p, m)).await
        }
        "system_setting" => setting(&s("setting"), input["on"].as_bool(), input["value"].as_u64()).await,
        "screenshot" => screenshot(s("target") == "window").await,
        "power" => power(&s("action"), input["confirmed"].as_bool() == Some(true)).await,
        "window_arrange" => {
            let (a, w) = (s("action"), s("window"));
            blocking(move || arrange(&a, &w)).await
        }
        "system_info" => system_info(&s("topic")).await,
        "calc" => calc(&s("expression")).map(|v| format!("{} = {}", s("expression"), format_number(v))),
        "clipboard_history" => clipboard_history(input["restore"].as_u64().map(|v| v as usize)),
        _ => return None,
    };
    Some(match result {
        Ok(msg) => (msg, false),
        Err(msg) => (msg, true),
    })
}

async fn blocking(f: impl FnOnce() -> Result<String, String> + Send + 'static) -> Result<String, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

// ---------- 音量 ----------

pub fn set_volume(percent: Option<u32>, mute: Option<bool>) -> Result<String, String> {
    if percent.is_none() && mute.is_none() {
        return Err("要给出 percent 或 mute".into());
    }
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let e: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
        let device = e.GetDefaultAudioEndpoint(eRender, eConsole).map_err(|_| "没有找到播放设备".to_string())?;
        let vol: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None).map_err(|e| e.to_string())?;
        if let Some(p) = percent {
            vol.SetMasterVolumeLevelScalar(p as f32 / 100.0, std::ptr::null()).map_err(|e| e.to_string())?;
            if p > 0 && mute.is_none() {
                let _ = vol.SetMute(false, std::ptr::null());
            }
        }
        if let Some(m) = mute {
            vol.SetMute(m, std::ptr::null()).map_err(|e| e.to_string())?;
        }
        let now = (vol.GetMasterVolumeLevelScalar().unwrap_or(0.0) * 100.0).round();
        let muted = vol.GetMute().map(|b| b.as_bool()).unwrap_or(false);
        Ok(if muted { format!("已静音（音量 {now}%）") } else { format!("音量已设为 {now}%") })
    }
}

// ---------- 系统设置 ----------

const RADIO: &str = r#"Add-Type -AssemblyName System.Runtime.WindowsRuntime
$asTask=([System.WindowsRuntimeSystemExtensions].GetMethods()|?{$_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1'})[0]
function Await($op,$t){ $x=$asTask.MakeGenericMethod($t).Invoke($null,@($op)); $x.Wait(-1)|Out-Null; $x.Result }
[Windows.Devices.Radios.Radio,Windows.System.Devices,ContentType=WindowsRuntime]|Out-Null
Await ([Windows.Devices.Radios.Radio]::RequestAccessAsync()) ([Windows.Devices.Radios.RadioAccessStatus])|Out-Null
$all=Await ([Windows.Devices.Radios.Radio]::GetRadiosAsync()) ([System.Collections.Generic.IReadOnlyList[Windows.Devices.Radios.Radio]])
$r=@($all|?{$_.Kind -eq $env:AERI_KIND})
if($r.Count -eq 0){ 'NONE'; exit }
foreach($x in $r){ Await ($x.SetStateAsync($env:AERI_STATE)) ([Windows.Devices.Radios.RadioAccessStatus]) }"#;

const BRIGHTNESS: &str = r#"$m=Get-CimInstance -Namespace root/WMI -ClassName WmiMonitorBrightnessMethods -ErrorAction SilentlyContinue
if(-not $m){ 'NONE'; exit }
$m | ForEach-Object { Invoke-CimMethod -InputObject $_ -MethodName WmiSetBrightness -Arguments @{Timeout=1; Brightness=[byte]$env:AERI_VALUE} | Out-Null }; 'OK'"#;

const DARK: &str = r#"$k='HKCU:\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize'
Set-ItemProperty $k AppsUseLightTheme ([int]$env:AERI_LIGHT); Set-ItemProperty $k SystemUsesLightTheme ([int]$env:AERI_LIGHT); 'OK'"#;

async fn setting(name: &str, on: Option<bool>, value: Option<u64>) -> Result<String, String> {
    let page = |uri: &str, what: &str| {
        crate::sys::shell_open(uri);
        Ok(format!("Windows 不允许程序直接切换{what}，已打开对应的设置页，请点一下开关"))
    };
    match name {
        "brightness" => {
            let v = value.ok_or("要给出亮度 value（0-100）")?.min(100);
            let out = ps::run(BRIGHTNESS, &[("AERI_VALUE", v.to_string())], Duration::from_secs(10)).await?;
            if out == "NONE" {
                return page("ms-settings:display", "外接显示器的亮度（只能用显示器上的按键调）");
            }
            Ok(format!("亮度已设为 {v}%"))
        }
        "dark_mode" => {
            let on = on.ok_or("要给出 on")?;
            ps::run(DARK, &[("AERI_LIGHT", if on { "0" } else { "1" }.into())], Duration::from_secs(10)).await?;
            Ok(if on { "已切换到深色模式" } else { "已切换到浅色模式" }.into())
        }
        "wifi" | "bluetooth" => {
            let on = on.ok_or("要给出 on")?;
            let (kind, label) = if name == "wifi" { ("WiFi", "Wi-Fi") } else { ("Bluetooth", "蓝牙") };
            let env = [("AERI_KIND", kind.to_string()), ("AERI_STATE", if on { "On" } else { "Off" }.into())];
            let out = ps::run(RADIO, &env, Duration::from_secs(15)).await.map_err(|e| format!("切换{label}失败：{e}"))?;
            if out == "NONE" {
                return Err(format!("这台电脑没有{label}"));
            }
            if out.lines().any(|l| l.trim() != "Allowed") {
                return Err(format!("Windows 不允许切换{label}（{}），可以在设置里打开“允许应用控制设备无线”", out.replace('\n', " ")));
            }
            Ok(format!("{label}已{}", if on { "打开" } else { "关闭" }))
        }
        "night_light" => page("ms-settings:nightlight", "夜间模式"),
        "do_not_disturb" => page("ms-settings:notifications", "勿扰模式"),
        "display" => page("ms-settings:display", "显示设置"),
        "sound" => page("ms-settings:sound", "声音设置"),
        other => Err(format!("不支持的设置：{other}")),
    }
}

// ---------- 截图 ----------

const SHOT: &str = r#"Add-Type -AssemblyName System.Windows.Forms,System.Drawing
Add-Type -Name D -Namespace W -MemberDefinition '[DllImport("user32.dll")] public static extern bool SetProcessDPIAware();'
[W.D]::SetProcessDPIAware()|Out-Null
if($env:AERI_W -eq '0'){ $v=[Windows.Forms.SystemInformation]::VirtualScreen; $x=$v.X;$y=$v.Y;$w=$v.Width;$h=$v.Height }
else { $x=[int]$env:AERI_X;$y=[int]$env:AERI_Y;$w=[int]$env:AERI_W;$h=[int]$env:AERI_H }
$b=New-Object Drawing.Bitmap $w,$h; $g=[Drawing.Graphics]::FromImage($b); $g.CopyFromScreen($x,$y,0,0,$b.Size)
New-Item -ItemType Directory -Force (Split-Path $env:AERI_PATH)|Out-Null
$b.Save($env:AERI_PATH,[Drawing.Imaging.ImageFormat]::Png); [Windows.Forms.Clipboard]::SetImage($b); 'OK'"#;

async fn screenshot(window: bool) -> Result<String, String> {
    let (mut x, mut y, mut w, mut h) = (0, 0, 0, 0);
    let mut title = String::new();
    if window {
        let (hwnd, t) = screen::front_window().ok_or("没有找到正在看的窗口")?;
        let r = window_rect(hwnd);
        (x, y, w, h) = (r.left, r.top, r.right - r.left, r.bottom - r.top);
        title = t;
    }
    let dir = std::env::var_os("USERPROFILE").map(std::path::PathBuf::from).unwrap_or_default().join("Pictures").join("Screenshots");
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let file = dir.join(format!("Aeri-{}{:02}{:02}-{:02}{:02}{:02}.png", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond));
    let env = [
        ("AERI_X", x.to_string()),
        ("AERI_Y", y.to_string()),
        ("AERI_W", w.to_string()),
        ("AERI_H", h.to_string()),
        ("AERI_PATH", file.to_string_lossy().into_owned()),
    ];
    ps::run(SHOT, &env, Duration::from_secs(15)).await.map_err(|e| format!("截图失败：{e}"))?;
    let what = if window { format!("窗口「{title}」") } else { "整个屏幕".into() };
    Ok(format!("已截取{what}，复制到了剪贴板，文件保存在 {}", file.display()))
}

fn window_rect(hwnd: HWND) -> RECT {
    let mut r = RECT::default();
    unsafe {
        // 扩展边框不含阴影，截出来没有黑边
        if DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut _ as *mut _, std::mem::size_of::<RECT>() as u32).is_err() {
            let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut r);
        }
    }
    r
}

// ---------- 电源 ----------

async fn power(action: &str, confirmed: bool) -> Result<String, String> {
    let shutdown = |args: &[&str]| {
        use crate::config::NoWindow;
        std::process::Command::new("shutdown.exe").args(args).creation_flags_no_window().status().map(|s| s.success()).unwrap_or(false)
    };
    match action {
        "lock" => {
            unsafe { LockWorkStation() }.map_err(|e| format!("锁屏失败：{e}"))?;
            Ok("已锁屏".into())
        }
        "sleep" => {
            let script = "Add-Type -AssemblyName System.Windows.Forms; [Windows.Forms.Application]::SetSuspendState('Suspend',$false,$false)|Out-Null";
            // 先回复再睡眠：延迟 2 秒，让播报说完
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(3)).await;
                let _ = ps::run(script, &[], Duration::from_secs(10)).await;
            });
            Ok("电脑马上进入睡眠".into())
        }
        "shutdown" | "restart" => {
            let label = if action == "shutdown" { "关机" } else { "重启" };
            if !confirmed {
                return Err(format!("{label}会关掉所有程序，先问用户确认，用户同意后再带上 confirmed=true 调用"));
            }
            let flag = if action == "shutdown" { "/s" } else { "/r" };
            if !shutdown(&[flag, "/t", "30"]) {
                return Err(format!("{label}失败，可能已经有一个关机计划，可以先 cancel"));
            }
            Ok(format!("30 秒后{label}，要取消就说“取消关机”"))
        }
        "cancel" => {
            if shutdown(&["/a"]) { Ok("已取消关机或重启".into()) } else { Err("现在没有要关机或重启的计划".into()) }
        }
        other => Err(format!("未知操作：{other}")),
    }
}

// ---------- 窗口 ----------

fn arrange(action: &str, window: &str) -> Result<String, String> {
    if action == "show_desktop" {
        press_keys(&[VK_LWIN, VK_D]);
        return Ok("已显示桌面".into());
    }
    let (hwnd, title) = if window.is_empty() { screen::front_window() } else { screen::find_window(window) }
        .ok_or_else(|| if window.is_empty() { "没有找到正在看的窗口".to_string() } else { format!("没有找到标题包含「{window}」的窗口") })?;
    unsafe {
        match action {
            "maximize" => _ = ShowWindow(hwnd, SW_MAXIMIZE),
            "minimize" => _ = ShowWindow(hwnd, SW_MINIMIZE),
            "restore" => _ = ShowWindow(hwnd, SW_RESTORE),
            "left" | "right" => {
                let area = work_area(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST));
                let half = (area.right - area.left) / 2;
                let x = if action == "left" { area.left } else { area.left + half };
                place(hwnd, x, area.top, half, area.bottom - area.top);
            }
            "next_monitor" => {
                let monitors = monitors();
                if monitors.len() < 2 {
                    return Err("只有一块屏幕".into());
                }
                let cur = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                let i = monitors.iter().position(|m| *m == cur).unwrap_or(0);
                let (from, to) = (work_area(cur), work_area(monitors[(i + 1) % monitors.len()]));
                let was_max = IsZoomed(hwnd).as_bool();
                let r = window_rect(hwnd);
                // 保持在屏幕里的相对位置，大小不超过新屏幕
                let w = (r.right - r.left).min(to.right - to.left);
                let h = (r.bottom - r.top).min(to.bottom - to.top);
                let x = (to.left + (r.left - from.left)).clamp(to.left, to.right - w);
                let y = (to.top + (r.top - from.top)).clamp(to.top, to.bottom - h);
                place(hwnd, x, y, w, h);
                if was_max {
                    let _ = ShowWindow(hwnd, SW_MAXIMIZE);
                }
            }
            other => return Err(format!("未知操作：{other}")),
        }
    }
    let verb = match action {
        "maximize" => "已最大化",
        "minimize" => "已最小化",
        "restore" => "已还原",
        "left" => "已放到左半边",
        "right" => "已放到右半边",
        _ => "已移到另一块屏幕",
    };
    Ok(format!("{verb}：{title}"))
}

unsafe fn place(hwnd: HWND, x: i32, y: i32, w: i32, h: i32) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

fn work_area(m: HMONITOR) -> RECT {
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe {
        let _ = GetMonitorInfoW(m, &mut info);
    }
    info.rcWork
}

fn monitors() -> Vec<HMONITOR> {
    unsafe extern "system" fn collect(m: HMONITOR, _: HDC, _: *mut RECT, lparam: LPARAM) -> BOOL {
        unsafe { (*(lparam.0 as *mut Vec<HMONITOR>)).push(m) };
        true.into()
    }
    let mut list: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut list as *mut _ as isize));
    }
    list
}

pub fn show_desktop() {
    press_keys(&[VK_LWIN, VK_D]);
}

fn press_keys(keys: &[VIRTUAL_KEY]) {
    let make = |vk, flags| INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, dwFlags: flags, ..Default::default() } } };
    let mut inputs: Vec<INPUT> = keys.iter().map(|k| make(*k, Default::default())).collect();
    inputs.extend(keys.iter().rev().map(|k| make(*k, KEYEVENTF_KEYUP)));
    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
}

// ---------- 系统状态 ----------

const CPU: &str = r#"$t=(Get-CimInstance Win32_Processor | Measure-Object LoadPercentage -Average).Average
$p=Get-CimInstance Win32_PerfFormattedData_PerfProc_Process | ?{ $_.Name -notin '_Total','Idle' } | Sort-Object PercentProcessorTime -Descending | Select-Object -First 5
$n=[Environment]::ProcessorCount
"CPU 总占用 $([int]$t)%"
$p | ForEach-Object { "$($_.Name -replace '#\d+$','')：CPU $([math]::Round($_.PercentProcessorTime/$n))%，内存 $([math]::Round($_.WorkingSetPrivate/1MB)) MB" }"#;

const NET: &str = r#"$a=Get-NetAdapter -ErrorAction SilentlyContinue | ?{ $_.Status -eq 'Up' }
if(-not $a){ '没有连接网络'; exit }
$a | ForEach-Object { "$($_.Name)：已连接，速率 $($_.LinkSpeed)" }
$w=netsh wlan show interfaces 2>$null
$ssid=($w | Select-String '^\s*SSID\s*:' | Select-Object -First 1) -replace '^.*?:\s*',''
$sig=($w | Select-String '(信号|Signal)\s*:' | Select-Object -First 1) -replace '^.*?:\s*',''
if($ssid){ "Wi-Fi：$ssid，信号 $sig" }
try { $null=Invoke-WebRequest 'https://www.baidu.com' -UseBasicParsing -TimeoutSec 5 -Method Head; '外网：可以访问' } catch { '外网：访问不了' }"#;

async fn system_info(topic: &str) -> Result<String, String> {
    match topic {
        "cpu" => ps::run(CPU, &[], Duration::from_secs(20)).await,
        "network" => ps::run(NET, &[], Duration::from_secs(20)).await,
        _ => Ok(overview()),
    }
}

fn overview() -> String {
    let mut lines = Vec::new();
    unsafe {
        let mut p = SYSTEM_POWER_STATUS::default();
        if GetSystemPowerStatus(&mut p).is_ok() && p.BatteryFlag != 128 && p.BatteryLifePercent != 255 {
            let charging = if p.ACLineStatus == 1 { "，正在充电" } else { "" };
            lines.push(format!("电量 {}%{charging}", p.BatteryLifePercent));
        } else {
            lines.push("没有电池（台式机或接着电源）".into());
        }
        let mut m = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        if GlobalMemoryStatusEx(&mut m).is_ok() {
            let gb = |b: u64| b as f64 / 1024f64.powi(3);
            lines.push(format!("内存已用 {}%（{:.1} / {:.1} GB）", m.dwMemoryLoad, gb(m.ullTotalPhys - m.ullAvailPhys), gb(m.ullTotalPhys)));
        }
        let mask = GetLogicalDrives();
        for i in 0..26 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let root = format!("{}:\\", (b'A' + i as u8) as char);
            let (mut free, mut total) = (0u64, 0u64);
            if GetDiskFreeSpaceExW(&HSTRING::from(&root), None, Some(&mut total), Some(&mut free)).is_ok() && total > 0 {
                lines.push(format!("{} 盘剩余 {} / {}", &root[..1], super::files::size_text(free), super::files::size_text(total)));
            }
        }
        let mins = GetTickCount64() / 60_000;
        lines.push(format!("已开机 {} 小时 {} 分钟", mins / 60, mins % 60));
    }
    lines.join("\n")
}

// ---------- 计算器 ----------

pub fn calc(expr: &str) -> Result<f64, String> {
    let s: String = expr
        .chars()
        .map(|c| match c {
            '×' | 'x' | 'X' => '*',
            '÷' => '/',
            '（' => '(',
            '）' => ')',
            '，' | ',' => ' ',
            c => c,
        })
        .collect();
    let mut p = Parser { s: s.as_bytes(), i: 0 };
    let v = p.expr()?;
    p.ws();
    if p.i < p.s.len() {
        return Err(format!("看不懂表达式「{expr}」里的「{}」", String::from_utf8_lossy(&p.s[p.i..])));
    }
    if !v.is_finite() {
        return Err("结果不是有限的数（比如除以了 0）".into());
    }
    Ok(v)
}

fn format_number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{v:.10}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }
    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn expr(&mut self) -> Result<f64, String> {
        let mut v = self.term()?;
        loop {
            if self.eat(b'+') {
                v += self.term()?;
            } else if self.eat(b'-') {
                v -= self.term()?;
            } else {
                return Ok(v);
            }
        }
    }
    fn term(&mut self) -> Result<f64, String> {
        let mut v = self.unary()?;
        loop {
            if self.eat(b'*') {
                v *= self.unary()?;
            } else if self.eat(b'/') {
                v /= self.unary()?;
            } else if self.eat(b'%') {
                v %= self.unary()?;
            } else {
                return Ok(v);
            }
        }
    }
    fn power(&mut self) -> Result<f64, String> {
        // -3^2 = -9：乘方比负号优先，指数可以带负号
        let base = self.atom()?;
        if self.eat(b'^') { Ok(base.powf(self.unary()?)) } else { Ok(base) }
    }
    fn unary(&mut self) -> Result<f64, String> {
        if self.eat(b'-') {
            return Ok(-self.unary()?);
        }
        if self.eat(b'+') {
            return self.unary();
        }
        self.power()
    }
    fn atom(&mut self) -> Result<f64, String> {
        self.ws();
        if self.eat(b'(') {
            let v = self.expr()?;
            if !self.eat(b')') {
                return Err("括号没有配对".into());
            }
            return Ok(v);
        }
        let start = self.i;
        if self.s.get(self.i).is_some_and(|c| c.is_ascii_digit() || *c == b'.') {
            while self.s.get(self.i).is_some_and(|c| c.is_ascii_digit() || *c == b'.') {
                self.i += 1;
            }
            let num: f64 = std::str::from_utf8(&self.s[start..self.i]).unwrap().parse().map_err(|_| "数字写法不对".to_string())?;
            // 百分号紧跟数字时当百分比：50% = 0.5（取模要写成 a % b，中间有空格或后面跟数字）
            if self.s.get(self.i) == Some(&b'%') && !self.s.get(self.i + 1).is_some_and(|c| c.is_ascii_digit() || *c == b'(' || *c == b' ') {
                self.i += 1;
                return Ok(num / 100.0);
            }
            return Ok(num);
        }
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_alphabetic()) {
            self.i += 1;
        }
        let name = std::str::from_utf8(&self.s[start..self.i]).unwrap().to_lowercase();
        match name.as_str() {
            "pi" => return Ok(std::f64::consts::PI),
            "e" => return Ok(std::f64::consts::E),
            "" => return Err("表达式不完整".into()),
            _ => {}
        }
        let arg = self.atom()?;
        let rad = arg.to_radians();
        Ok(match name.as_str() {
            "sqrt" => arg.sqrt(),
            "abs" => arg.abs(),
            "round" => arg.round(),
            "floor" => arg.floor(),
            "ceil" => arg.ceil(),
            "ln" => arg.ln(),
            "log" => arg.log10(),
            "sin" => rad.sin(),
            "cos" => rad.cos(),
            "tan" => rad.tan(),
            other => return Err(format!("不支持的函数：{other}")),
        })
    }
}

// ---------- 剪贴板历史 ----------

static CLIPS: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static WATCH: Once = Once::new();

/// 后台每 0.7 秒看一眼剪贴板序号，变了就记下文字。只存在内存里，退出就没了
pub fn start_clipboard_watch() {
    WATCH.call_once(|| {
        std::thread::spawn(|| {
            let mut last = 0;
            loop {
                let seq = unsafe { GetClipboardSequenceNumber() };
                if seq != last {
                    last = seq;
                    if let Some(text) = read_clipboard() {
                        let mut clips = CLIPS.lock().unwrap();
                        clips.retain(|c| *c != text);
                        clips.push_front(text);
                        clips.truncate(CLIP_KEEP);
                    }
                }
                std::thread::sleep(Duration::from_millis(700));
            }
        });
    });
}

fn read_clipboard() -> Option<String> {
    unsafe {
        OpenClipboard(None).ok()?;
        let text = (|| {
            let h = GetClipboardData(CF_UNICODETEXT).ok()?;
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if p.is_null() {
                return None;
            }
            let mut len = 0;
            while *p.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
            let _ = GlobalUnlock(g);
            Some(s)
        })();
        let _ = CloseClipboard();
        text.filter(|t| !t.trim().is_empty())
    }
}

fn write_clipboard(text: &str) -> Result<(), String> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        OpenClipboard(None).map_err(|e| e.to_string())?;
        let r = (|| {
            EmptyClipboard().map_err(|e| e.to_string())?;
            let g = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2).map_err(|e| e.to_string())?;
            let p = GlobalLock(g) as *mut u16;
            std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
            let _ = GlobalUnlock(g);
            SetClipboardData(CF_UNICODETEXT, Some(HANDLE(g.0))).map_err(|e| e.to_string())?;
            Ok(())
        })();
        let _ = CloseClipboard();
        r
    }
}

fn clipboard_history(restore: Option<usize>) -> Result<String, String> {
    let clips = CLIPS.lock().unwrap().clone();
    if clips.is_empty() {
        return Ok("Aeri 启动后还没有复制过文字".into());
    }
    if let Some(n) = restore {
        let text = clips.get(n.wrapping_sub(1)).ok_or(format!("只有 {} 条记录", clips.len()))?;
        write_clipboard(text).map_err(|e| format!("写剪贴板失败：{e}"))?;
        return Ok(format!("已把第 {n} 条放回剪贴板，可以粘贴了：{}", short(text)));
    }
    Ok(clips.iter().enumerate().map(|(i, c)| format!("{}. {}", i + 1, short(c))).collect::<Vec<_>>().join("\n"))
}

fn short(s: &str) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > 80 { format!("{}…", one.chars().take(80).collect::<String>()) } else { one }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculates() {
        assert_eq!(calc("1+2*3").unwrap(), 7.0);
        assert_eq!(calc("(1+2)×3").unwrap(), 9.0);
        assert_eq!(calc("2^10").unwrap(), 1024.0);
        assert_eq!(calc("-3^2").unwrap(), -9.0);
        assert_eq!(calc("200*15%").unwrap(), 30.0);
        assert_eq!(calc("10 % 3").unwrap(), 1.0);
        assert_eq!(calc("sqrt(16)+abs(-2)").unwrap(), 6.0);
        assert!((calc("sin(30)").unwrap() - 0.5).abs() < 1e-9);
        assert_eq!(format_number(calc("1/3").unwrap()), "0.3333333333");
        assert_eq!(format_number(calc("0.1+0.2").unwrap()), "0.3");
        assert!(calc("1/0").is_err());
        assert!(calc("1+").is_err());
        assert!(calc("foo(1)").is_err());
    }

    #[test]
    fn overview_has_memory_and_disk() {
        let o = overview();
        assert!(o.contains("内存") && o.contains("盘剩余"), "{o}");
    }

    /// 手动测试：先开一个标题含 Downloads 的资源管理器窗口
    #[test]
    #[ignore]
    fn arranges_window() {
        println!("{:?}", arrange("left", "Downloads"));
        println!("{:?}", arrange("maximize", "Downloads"));
        println!("{:?}", arrange("restore", "Downloads"));
        println!("{:?}", clipboard_history(None));
    }

    /// 手动测试：`cargo test system_manual -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn system_manual() {
        println!("{}", system_info("cpu").await.unwrap());
        println!("{}", system_info("network").await.unwrap());
        println!("{:?}", blocking(|| set_volume(None, Some(false))).await);
        println!("{:?}", screenshot(true).await);
    }
}
