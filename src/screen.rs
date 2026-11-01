//! 屏幕上下文：用户正在看什么。
//! 用户说“关闭当前直播”“这个视频暂停一下”时不会说是哪个浏览器的哪个页面，
//! 所以每次对话前读一下前台窗口（标题、程序、是否全屏），交给模型一起理解。
//! 只读窗口标题，不截图、不读页面内容，几毫秒就完成。

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetWindow, GetWindowLongPtrW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, WS_EX_TOOLWINDOW,
};
use windows::core::BOOL;

use crate::tools::process::image_path;

/// 除了前台窗口，再列几个后面的窗口，方便理解“刚才那个”
const MORE_WINDOWS: usize = 3;

struct Window {
    hwnd: HWND,
    title: String,
    exe: String,
    fullscreen: bool,
}

/// 用户能看到的顶层窗口，按从上到下的顺序；跳过助手自己的窗口。
/// 主界面被点开时它自己在最前面，跳过它，拿到的就是用户刚才在看的窗口。
fn visible_windows(limit: usize) -> Vec<Window> {
    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let list = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
        list.push(hwnd);
        true.into()
    }
    let mut hwnds: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut hwnds as *mut Vec<HWND> as isize));
    }
    let me = unsafe { GetCurrentProcessId() };
    let mut out = Vec::new();
    for hwnd in hwnds {
        if out.len() >= limit {
            break;
        }
        if let Some(w) = describe(hwnd, me) {
            out.push(w);
        }
    }
    out
}

fn describe(hwnd: HWND, me: u32) -> Option<Window> {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
        // 对话框、工具窗口不算；UWP 挂起的窗口“可见”但其实被藏起来了（cloaked）
        if GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid()) {
            return None;
        }
        if GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
            return None;
        }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4);
        if cloaked != 0 {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == me {
            return None;
        }
        let mut buf = [0u16; 512];
        let n = GetWindowTextW(hwnd, &mut buf);
        // Edge 的标题里夹着零宽空格（Microsoft\u{200b} Edge），去掉免得干扰模型
        let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]).replace('\u{200b}', "").trim().to_string();
        // 桌面（Program Manager）和没有标题的窗口不算
        if title.is_empty() || title == "Program Manager" {
            return None;
        }
        let path = image_path(pid);
        // 后台智能体通过 MCP 调用时，读屏的是另一个 aeri 进程，主界面也要跳过
        let own = std::env::current_exe().ok().map(|p| p.to_string_lossy().to_lowercase());
        if own.is_some_and(|own| own == path.to_lowercase()) {
            return None;
        }
        let exe = path.rsplit(['\\', '/']).next().unwrap_or("").to_string();
        Some(Window { hwnd, title, exe, fullscreen: is_fullscreen(hwnd) })
    }
}

fn is_fullscreen(hwnd: HWND) -> bool {
    unsafe {
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return false;
        }
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut info).as_bool() {
            return false;
        }
        let m = info.rcMonitor;
        r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
    }
}

fn label(w: &Window) -> String {
    let mut s = format!("「{}」", w.title);
    match (w.exe.is_empty(), w.fullscreen) {
        (false, true) => s.push_str(&format!("（{}，全屏）", w.exe)),
        (false, false) => s.push_str(&format!("（{}）", w.exe)),
        (true, true) => s.push_str("（全屏）"),
        (true, false) => {}
    }
    s
}

/// 用户正在看的窗口（句柄和标题），给读屏工具用
pub fn front_window() -> Option<(HWND, String)> {
    visible_windows(1).into_iter().next().map(|w| (w.hwnd, w.title))
}

/// 标题包含 name 的窗口（不区分大小写），按从上到下找第一个；直接枚举窗口，几毫秒就完成
pub fn find_window(name: &str) -> Option<(HWND, String)> {
    let needle = name.to_lowercase();
    visible_windows(usize::MAX).into_iter().find(|w| w.title.to_lowercase().contains(&needle)).map(|w| (w.hwnd, w.title))
}

/// 一句话描述屏幕上正在干什么；读不到时返回 None
pub fn context() -> Option<String> {
    let windows = visible_windows(MORE_WINDOWS + 1);
    let (front, rest) = windows.split_first()?;
    let mut text = format!("前台窗口{}", label(front));
    if !rest.is_empty() {
        let rest: Vec<String> = rest.iter().map(label).collect();
        text.push_str(&format!("；后面还开着{}", rest.join("、")));
    }
    Some(text)
}

/// 交给前台模型的说明：用户说“当前、这个、正在看的”时指什么
pub fn hint() -> String {
    match context() {
        Some(ctx) => format!(
            "【屏幕上下文，系统自动读取，不是用户说的话】用户说话时屏幕上：{ctx}。用户说“当前、这个、正在放的、这里”而没指明是哪个时，就是指前台窗口；要知道里面具体是什么内容，用 screen_read 看。"
        ),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_screen() {
        // 有桌面会话时至少能读到一个窗口；在没有桌面的环境里也不应该出错
        println!("{:?}", super::context());
    }
}
