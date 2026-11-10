//! Offline OCR around the mouse pointer, for content that has no accessible text:
//! terminals drawn on a canvas, images, games, remote desktops.
//! Uses the OCR engine built into Windows (Windows.Media.Ocr), so nothing leaves the PC.

use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, GetDC, GetDIBits, ReleaseDC, SRCCOPY, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

/// How far around the pointer to look (physical pixels).
const HALF_W: i32 = 700;
const HALF_H: i32 = 140;

/// Text near the pointer: the line under it first, then the lines around it in reading order.
pub fn text_near(x: i32, y: i32) -> Option<String> {
    let (left, top, w, h) = region(x, y);
    let pixels = capture(left, top, w, h)?;
    let lines = recognize(&pixels, w, h)?;
    if lines.is_empty() {
        return None;
    }
    let (cx, cy) = ((x - left) as f32, (y - top) as f32);
    // The line whose box is closest to the pointer
    let dist = |l: &Line| {
        let dx = if cx < l.x0 { l.x0 - cx } else if cx > l.x1 { cx - l.x1 } else { 0.0 };
        let dy = if cy < l.y0 { l.y0 - cy } else if cy > l.y1 { cy - l.y1 } else { 0.0 };
        dx * 4.0 + dy * 8.0 // prefer the same row
    };
    let pointed = lines.iter().min_by(|a, b| dist(a).total_cmp(&dist(b)))?.text.clone();
    let around: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
    Some(format!("{pointed}\n\n{}", around.join("\n")))
}

struct Line {
    text: String,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

fn region(x: i32, y: i32) -> (i32, i32, i32, i32) {
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    let left = (x - HALF_W).max(vx);
    let top = (y - HALF_H).max(vy);
    let right = (x + HALF_W).min(vx + vw);
    let bottom = (y + HALF_H).min(vy + vh);
    (left, top, (right - left).max(1), (bottom - top).max(1))
}

/// BGRA pixels of a screen rectangle, top row first.
fn capture(left: i32, top: i32, w: i32, h: i32) -> Option<Vec<u8>> {
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp.into());
        let ok = BitBlt(mem, 0, 0, w, h, Some(screen), left, top, SRCCOPY).is_ok();
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let rows = if ok {
            GetDIBits(mem, bmp, 0, h as u32, Some(buf.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS)
        } else {
            0
        };
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        (rows == h).then_some(buf)
    }
}

fn recognize(pixels: &[u8], w: i32, h: i32) -> Option<Vec<Line>> {
    let engine = OcrEngine::TryCreateFromUserProfileLanguages().ok()?;
    // Small text reads much better when enlarged; the OCR engine accepts up to 10000 px a side.
    let scale = if h < 400 { 2 } else { 1 };
    let (sw, sh) = (w * scale, h * scale);
    let data = if scale == 1 { pixels.to_vec() } else { upscale(pixels, w, h, scale) };
    let writer = DataWriter::new().ok()?;
    writer.WriteBytes(&data).ok()?;
    let buffer = writer.DetachBuffer().ok()?;
    let bitmap = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
        &buffer,
        BitmapPixelFormat::Bgra8,
        sw,
        sh,
        BitmapAlphaMode::Ignore,
    )
    .ok()?;
    let result = engine.RecognizeAsync(&bitmap).ok()?.join().ok()?;
    let mut out = Vec::new();
    for line in result.Lines().ok()? {
        let text = line.Text().ok()?.to_string();
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for word in line.Words().ok()? {
            let r = word.BoundingRect().ok()?;
            x0 = x0.min(r.X);
            y0 = y0.min(r.Y);
            x1 = x1.max(r.X + r.Width);
            y1 = y1.max(r.Y + r.Height);
        }
        let s = scale as f32;
        let text = tidy(&text);
        if !text.is_empty() {
            out.push(Line { text, x0: x0 / s, y0: y0 / s, x1: x1 / s, y1: y1 / s });
        }
    }
    Some(out)
}

fn upscale(src: &[u8], w: i32, h: i32, k: i32) -> Vec<u8> {
    let (w, h, k) = (w as usize, h as usize, k as usize);
    let mut out = vec![0u8; w * k * h * k * 4];
    let row = w * k * 4;
    for y in 0..h {
        for x in 0..w {
            let p = &src[(y * w + x) * 4..(y * w + x) * 4 + 4];
            for dy in 0..k {
                for dx in 0..k {
                    let o = (y * k + dy) * row + (x * k + dx) * 4;
                    out[o..o + 4].copy_from_slice(p);
                }
            }
        }
    }
    out
}

/// The Chinese OCR model puts a space between every character; keep spaces only between Latin words.
fn tidy(text: &str) -> String {
    let wide = |c: char| (c as u32) >= 0x2E80;
    let chars: Vec<char> = text.trim().chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == ' ' {
            let prev = out.chars().last();
            let next = chars.get(i + 1).copied();
            if prev.is_some_and(wide) || next.is_some_and(wide) || next == Some(' ') {
                continue;
            }
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn joins_chinese_keeps_english_spaces() {
        assert_eq!(super::tidy("轻 量 、 常 驻 的 中 文 语 音 助 手"), "轻量、常驻的中文语音助手");
        assert_eq!(super::tidy("Use /btw to ask 一 个 问 题"), "Use /btw to ask一个问题");
    }
}
