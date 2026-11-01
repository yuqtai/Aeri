//! 语音对话期间把其他程序的声音调低、结束后恢复，免得麦克风把电脑里放的视频、音乐也当成用户在说话。
//! 调的是每个程序在“音量合成器”里的音量（VOA 自己的播报不动）。Windows 会记住这个音量，
//! 所以调低时把原音量写到文件里：万一 VOA 中途退出，下次启动先恢复。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::core::{Interface, PWSTR};

use crate::config;

/// 调低到原来的多少
const DUCK_TO: f32 = 0.2;
/// 调低期间多久看一次有没有新出声的程序
const RESCAN_EVERY: Duration = Duration::from_secs(1);

pub struct Ducker {
    tx: Option<mpsc::Sender<bool>>,
    thread: Option<JoinHandle<()>>,
}

impl Ducker {
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::Builder::new().name("duck".into()).spawn(move || run(rx)).ok();
        Self { tx: Some(tx), thread }
    }

    /// true：调低其他声音；false：恢复
    pub fn set(&self, on: bool) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(on);
        }
    }
}

impl Drop for Ducker {
    fn drop(&mut self) {
        self.tx = None; // 线程收到断开后恢复音量再退出
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// 一个被调低的会话
struct Ducked {
    volume: ISimpleAudioVolume,
    /// 同一个程序跨重启不变，用来在下次启动时恢复
    app: String,
    original: f32,
    lowered: f32,
}

fn run(rx: mpsc::Receiver<bool>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    restore_leftovers();
    let mut ducked: HashMap<String, Ducked> = HashMap::new();
    let mut on = false;
    loop {
        match rx.recv_timeout(RESCAN_EVERY) {
            Ok(v) => on = v,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if on {
            let before = ducked.len();
            duck_new_sessions(&mut ducked);
            if ducked.len() != before {
                save(&ducked);
            }
        } else if !ducked.is_empty() {
            restore(&mut ducked);
        }
    }
    restore(&mut ducked);
    unsafe { CoUninitialize() };
}

fn duck_new_sessions(ducked: &mut HashMap<String, Ducked>) {
    for s in sessions() {
        if ducked.contains_key(&s.instance) {
            continue;
        }
        let Ok(original) = (unsafe { s.volume.GetMasterVolume() }) else { continue };
        let lowered = original * DUCK_TO;
        if unsafe { s.volume.SetMasterVolume(lowered, std::ptr::null()) }.is_ok() {
            ducked.insert(s.instance, Ducked { volume: s.volume, app: s.app, original, lowered });
        }
    }
}

fn restore(ducked: &mut HashMap<String, Ducked>) {
    for (_, d) in ducked.drain() {
        // 期间用户自己调过这个程序的音量就不动了
        if unsafe { d.volume.GetMasterVolume() }.is_ok_and(|v| (v - d.lowered).abs() < 0.01) {
            let _ = unsafe { d.volume.SetMasterVolume(d.original, std::ptr::null()) };
        }
    }
    let _ = std::fs::remove_file(state_path());
}

/// 上次调低后没来得及恢复（VOA 被结束、崩溃）：按程序把音量恢复回去
fn restore_leftovers() {
    let Ok(text) = std::fs::read_to_string(state_path()) else { return };
    let saved: Vec<(String, f32, f32)> = serde_json::from_str(&text).unwrap_or_default();
    for s in sessions() {
        if let Some((_, original, lowered)) = saved.iter().find(|(app, ..)| *app == s.app) {
            if unsafe { s.volume.GetMasterVolume() }.is_ok_and(|v| (v - lowered).abs() < 0.01) {
                let _ = unsafe { s.volume.SetMasterVolume(*original, std::ptr::null()) };
            }
        }
    }
    let _ = std::fs::remove_file(state_path());
}

fn save(ducked: &HashMap<String, Ducked>) {
    let saved: Vec<(&str, f32, f32)> = ducked.values().map(|d| (d.app.as_str(), d.original, d.lowered)).collect();
    if let Ok(text) = serde_json::to_string(&saved) {
        let _ = std::fs::write(state_path(), text);
    }
}

fn state_path() -> PathBuf {
    config::app_dir().join("ducked.json")
}

struct Session {
    volume: ISimpleAudioVolume,
    instance: String,
    app: String,
}

/// 所有播放设备上其他程序的音频会话（不含 VOA 自己和系统提示音）
fn sessions() -> Vec<Session> {
    let own = std::process::id();
    let mut out = Vec::new();
    unsafe {
        let Ok(enumerator) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else { return out };
        let Ok(devices) = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) else { return out };
        for d in 0..devices.GetCount().unwrap_or(0) {
            let Ok(device) = devices.Item(d) else { continue };
            let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
            let Ok(list) = manager.GetSessionEnumerator() else { continue };
            for i in 0..list.GetCount().unwrap_or(0) {
                let Ok(control) = list.GetSession(i) else { continue };
                let Ok(control) = control.cast::<IAudioSessionControl2>() else { continue };
                if control.IsSystemSoundsSession() == S_OK || control.GetProcessId().is_ok_and(|pid| pid == own || pid == 0) {
                    continue;
                }
                let (Ok(instance), Ok(app)) = (control.GetSessionInstanceIdentifier(), control.GetSessionIdentifier()) else { continue };
                let Ok(volume) = control.cast::<ISimpleAudioVolume>() else { continue };
                out.push(Session { volume, instance: take_string(instance), app: take_string(app) });
            }
        }
    }
    out
}

fn take_string(p: PWSTR) -> String {
    unsafe {
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volumes() -> HashMap<String, f32> {
        sessions().into_iter().filter_map(|s| Some((s.instance, unsafe { s.volume.GetMasterVolume() }.ok()?))).collect()
    }

    /// 需要另一个程序正在出声：cargo test ducks_and_restores -- --ignored --nocapture
    #[test]
    #[ignore]
    fn ducks_and_restores() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let before = volumes();
        assert!(!before.is_empty(), "没有其他程序的音频会话");
        let ducker = Ducker::start();
        ducker.set(true);
        std::thread::sleep(Duration::from_millis(500));
        let during = volumes();
        assert!(state_path().exists());
        ducker.set(false);
        std::thread::sleep(Duration::from_millis(500));
        let after = volumes();
        for (id, v) in &before {
            println!("{v:.2} -> {:.2} -> {:.2}  {id}", during[id], after[id]);
            assert!((during[id] - v * DUCK_TO).abs() < 0.01);
            assert!((after[id] - v).abs() < 0.01);
        }
        assert!(!state_path().exists());
    }
}
