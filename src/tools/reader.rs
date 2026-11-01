//! 读屏：用户正在看的内容（选中的文字、鼠标所指的那一块、整个可见区域）。
//! 这是 Aeri 自己的功能，agent-desktop 没有对应的命令，所以仍然直接调用 Windows UI Automation。
//! UIA 的 COM 对象不能跨线程，所以放在一个专用线程里，通过通道收发请求。

use std::sync::mpsc;

use serde_json::{Value, json};
use tokio::sync::oneshot;
use uiautomation::inputs::Mouse;
use uiautomation::patterns::UITextPattern;
use uiautomation::types::{ControlType, Point, TextPatternRangeEndpoint, TreeScope, UIProperty};
use uiautomation::{UIAutomation, UIElement, UITreeWalker};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::WindowsAndMessaging::{GA_ROOT, GetAncestor, WindowFromPoint};

use crate::screen;

const MAX_VISITED: usize = 3000;
/// 读屏时可见内容最多给多少字；鼠标所指的区块超过这个长度就不算“一块”了
const MAX_SCREEN_TEXT: usize = 6000;
const MAX_BLOCK_TEXT: usize = 2000;
/// 网页里表示“一块独立内容”的语义角色：帖子、列表项、评论、弹窗
const BLOCK_ROLES: &[&str] = &["article", "listitem", "comment", "dialog"];

type Job = (String, oneshot::Sender<(String, bool)>);

pub struct Reader {
    tx: mpsc::Sender<Job>,
}

impl Reader {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("screen-reader".into())
            .spawn(move || {
                let state = UIAutomation::new().ok().map(|automation| State { automation });
                for (window, reply) in rx {
                    let result = match &state {
                        Some(s) => s.read_screen(&window),
                        None => Err("UI Automation 初始化失败".into()),
                    };
                    let _ = reply.send(match result {
                        Ok(text) => (text, false),
                        Err(text) => (text, true),
                    });
                }
            })
            .expect("启动读屏线程失败");
        Self { tx }
    }

    pub async fn call(&self, input: &Value) -> (String, bool) {
        let window = input["window"].as_str().unwrap_or("").trim().to_string();
        let (reply_tx, reply_rx) = oneshot::channel();
        if self.tx.send((window, reply_tx)).is_err() {
            return ("读屏线程已退出".into(), true);
        }
        reply_rx.await.unwrap_or(("读屏没有返回结果".into(), true))
    }
}

struct State {
    automation: UIAutomation,
}

fn err<E: std::fmt::Display>(e: E) -> String {
    format!("读取失败：{e}")
}

impl State {
    fn children(walker: &UITreeWalker, el: &UIElement) -> Vec<UIElement> {
        let mut out = Vec::new();
        if let Ok(mut child) = walker.get_first_child(el) {
            loop {
                out.push(child.clone());
                match walker.get_next_sibling(&child) {
                    Ok(next) => child = next,
                    Err(_) => break,
                }
            }
        }
        out
    }

    /// 网页用文本模式（TextPattern）一次拿到可见文字，其他程序遍历控件名称。
    fn read_screen(&self, window: &str) -> Result<String, String> {
        let (hwnd, title) = if window.is_empty() {
            screen::front_window().ok_or("没有找到用户正在看的窗口")?
        } else {
            screen::find_window(window).ok_or_else(|| format!("没有找到标题包含“{window}”的窗口，可以先用 desktop_windows 查看"))?
        };
        let win = self.automation.element_from_handle(hwnd.into()).map_err(err)?;
        let walker = self.automation.get_control_view_walker().map_err(err)?;
        // 有正文区域（网页、文档）时只读正文，不读书签栏、工具栏这些程序自己的界面
        let doc = self.main_document(&win);
        let text = doc.as_ref().and_then(|d| d.get_pattern::<UITextPattern>().ok());
        let body = doc.unwrap_or_else(|| win.clone());
        let mut out = vec![format!("窗口：{title}")];

        if let Some(tp) = &text {
            let selected: Vec<String> =
                tp.get_selection().unwrap_or_default().iter().filter_map(|r| r.get_text(-1).ok()).collect();
            let selected = tidy(&selected.join("\n"));
            if !selected.is_empty() {
                out.push(format!("用户选中的文字：\n{}", clip(&selected, MAX_BLOCK_TEXT)));
            }
        }
        if let Some((role, block)) = self.block_under_cursor(&walker, &win, text.as_ref()) {
            let role = if role.is_empty() { String::new() } else { format!("（{role}）") };
            out.push(format!("鼠标所指的那一块{role}：\n{}", clip(&block, MAX_BLOCK_TEXT)));
        }
        let visible = text
            .as_ref()
            .and_then(|tp| visible_text(&body, tp))
            .map(|t| tidy(&t))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| self.subtree_text(&walker, &body, MAX_SCREEN_TEXT, true));
        out.push(format!("屏幕上能看到的内容：\n{}", clip(&visible, MAX_SCREEN_TEXT)));
        Ok(out.join("\n\n"))
    }

    /// 窗口里最大的文档区域（浏览器的网页、文档编辑器的正文）。
    /// 交给 UIA 按类型搜索，比自己一个个遍历控件快得多
    fn main_document(&self, win: &UIElement) -> Option<UIElement> {
        let cond = self
            .automation
            .create_property_condition(UIProperty::ControlType, (ControlType::Document as i32).into(), None)
            .ok()?;
        win.find_all(TreeScope::Descendants, &cond)
            .ok()?
            .into_iter()
            .filter(|d| !d.is_offscreen().unwrap_or(true))
            .max_by_key(|d| d.get_bounding_rectangle().map(|r| r.get_width() as i64 * r.get_height() as i64).unwrap_or(0))
    }

    /// 鼠标所指的那一块内容：从鼠标下的控件往上找，遇到帖子、列表项这类语义区块就停；
    /// 没有语义标记时取不超过长度上限的最大一层
    fn block_under_cursor(&self, walker: &UITreeWalker, win: &UIElement, text: Option<&UITextPattern>) -> Option<(String, String)> {
        let p = Mouse::get_cursor_pos().ok()?;
        // 鼠标下面露出来的必须是这个窗口（不是盖在它上面的别的窗口）
        let target: HWND = win.get_native_window_handle().ok()?.into();
        let under = unsafe { GetAncestor(WindowFromPoint(POINT { x: p.get_x(), y: p.get_y() }), GA_ROOT) };
        if under != target {
            return None;
        }
        let mut el = self.automation.element_from_point(p).ok()?;
        let mut best: Option<(String, String)> = None;
        for _ in 0..30 {
            let ct = el.get_control_type().ok()?;
            if matches!(ct, ControlType::Window | ControlType::Document | ControlType::Pane) && best.is_some() {
                break;
            }
            let content = text
                .and_then(|tp| tp.get_range_from_child(&el).ok())
                .and_then(|range| range.get_text(MAX_BLOCK_TEXT as i32 + 1).ok())
                .map(|t| tidy(&t))
                .unwrap_or_else(|| self.subtree_text(walker, &el, MAX_BLOCK_TEXT + 1, false));
            if content.chars().count() > MAX_BLOCK_TEXT {
                break;
            }
            let role = el.get_property_value(UIProperty::AriaRole).and_then(|v| v.get_string()).unwrap_or_default();
            if !content.is_empty() {
                best = Some((role.clone(), content));
                if BLOCK_ROLES.contains(&role.as_str()) {
                    break;
                }
            }
            if ct == ControlType::Window {
                break;
            }
            el = walker.get_parent(&el).ok()?;
        }
        best
    }

    /// 没有文本模式时，按顺序收集子树里控件的名称当作文字
    fn subtree_text(&self, walker: &UITreeWalker, root: &UIElement, limit: usize, visible_only: bool) -> String {
        let mut lines: Vec<String> = Vec::new();
        let mut len = 0;
        let mut stack = vec![root.clone()];
        let mut visited = 0;
        while let Some(el) = stack.pop() {
            visited += 1;
            if visited > MAX_VISITED || len > limit {
                break;
            }
            if visible_only && el.is_offscreen().unwrap_or(false) {
                continue;
            }
            let has_text = el.get_control_type().is_ok_and(|t| {
                matches!(t, ControlType::Text | ControlType::Hyperlink | ControlType::ListItem | ControlType::Edit | ControlType::DataItem)
            });
            if has_text {
                let name = el.get_name().unwrap_or_default();
                let name = name.trim();
                if !name.is_empty() {
                    len += name.chars().count() + 1;
                    lines.push(name.to_string());
                    continue; // 链接、列表项的名称已经包含了里面的文字，不再往下读，免得重复
                }
            }
            stack.extend(Self::children(walker, &el).into_iter().rev());
        }
        lines.join("\n")
    }
}

/// 正文区域里屏幕上能看到的文字：取左上角到右下角这一段，一次读出来
/// （按“可见片段”逐段读要几百次跨进程调用，网页上要两三秒）
fn visible_text(doc: &UIElement, tp: &UITextPattern) -> Option<String> {
    let r = doc.get_bounding_rectangle().ok()?;
    let start = tp.get_range_from_point(Point::new(r.get_left() + 2, r.get_top() + 2)).ok()?;
    let end = tp.get_range_from_point(Point::new(r.get_right() - 2, r.get_bottom() - 2)).ok()?;
    start.move_endpoint_by_range(TextPatternRangeEndpoint::End, &end, TextPatternRangeEndpoint::End).ok()?;
    start.get_text(MAX_SCREEN_TEXT as i32 * 2).ok()
}

/// 去掉图片等占位符（U+FFFC）和每行首尾空白，连续空行只留一个
fn tidy(text: &str) -> String {
    let text = text.replace(['\u{fffc}', '\u{200b}'], "").replace('\u{a0}', " ");
    let mut out: Vec<&str> = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() && out.last().is_none_or(|l| l.is_empty()) {
            continue;
        }
        out.push(line);
    }
    out.join("\n").trim().to_string()
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}……（后面还有，已截断）")
}

pub fn definitions() -> Vec<Value> {
    vec![json!({
        "name": "screen_read",
        "description": "看用户正在看的窗口（助手自己的窗口除外）：返回选中的文字、鼠标所指的那一块内容（比如一条帖子、一条评论）、屏幕上能看到的全部文字。用户提到屏幕上的东西（这个、当前、这条、这段、正在看的）时，先用它看清楚是什么再做。",
        "input_schema": {"type": "object", "properties": {"window": {"type": "string", "description": "要看别的窗口时填窗口标题的一部分；不填就是用户正在看的窗口"}}}
    })]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手动测试：开着 Edge 时运行 `cargo test reads_edge_window -- --ignored --nocapture`，看读到的内容和耗时
    #[test]
    #[ignore]
    fn reads_edge_window() {
        let s = State { automation: UIAutomation::new().unwrap() };
        let t = std::time::Instant::now();
        let out = s.read_screen("Edge").unwrap();
        println!("耗时 {:?}", t.elapsed());
        println!("{}", out.chars().take(1500).collect::<String>());
    }
}
