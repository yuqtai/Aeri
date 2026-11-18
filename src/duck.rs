//! 语音对话期间把其他程序的声音调低、结束后恢复，免得麦克风把电脑里放的视频、音乐也当成用户在说话。
//! 调的是每个程序在“音量合成器”里的音量（Aeri 自己的播报不动）。Windows 会记住这个音量，
//! 所以调低时把原音量写到文件里：万一 Aeri 中途退出，下次启动先恢复。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, mpsc};
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

enum Msg {
    Set(bool),
    RestoreNow(mpsc::SyncSender<()>),
}

static GLOBAL_TX: Mutex<Option<mpsc::Sender<Msg>>> = Mutex::new(None);

pub struct Ducker {
    tx: Option<mpsc::Sender<Msg>>,
    thread: Option<JoinHandle<()>>,
}

impl Ducker {
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel();
        *GLOBAL_TX.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx.clone());
        let thread = std::thread::Builder::new().name("duck".into()).spawn(move || run(rx)).ok();
        Self { tx: Some(tx), thread }
    }

    /// true：调低其他声音；false：恢复
    pub fn set(&self, on: bool) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Msg::Set(on));
        }
    }
}

impl Drop for Ducker {
    fn drop(&mut self) {
        *GLOBAL_TX.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.tx = None; // 线程收到断开后恢复音量再退出
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// 程序退出或更新重启前同步恢复所有被压低软件的音量
pub fn restore_all_now() {
    let tx = GLOBAL_TX.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(tx) = tx {
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        if tx.send(Msg::RestoreNow(ack_tx)).is_ok() && ack_rx.recv_timeout(Duration::from_millis(800)).is_ok() {
            return;
        }
    }
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let _ = restore_leftovers();
    if com.is_ok() {
        unsafe { CoUninitialize() };
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

fn run(rx: mpsc::Receiver<Msg>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let mut pending_failed = restore_leftovers();
    let mut ducked: HashMap<String, Ducked> = HashMap::new();
    let mut on = false;
    loop {
        match rx.recv_timeout(RESCAN_EVERY) {
            Ok(Msg::Set(v)) => on = v,
            Ok(Msg::RestoreNow(ack)) => {
                on = false;
                if !ducked.is_empty() {
                    restore(&mut ducked, &mut pending_failed);
                }
                retry_failed(&mut pending_failed);
                let _ = ack.send(());
                continue;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if on {
            let before = ducked.len();
            duck_new_sessions(&mut ducked);
            if ducked.len() != before {
                save(&ducked, &pending_failed);
            }
        } else if !ducked.is_empty() {
            restore(&mut ducked, &mut pending_failed);
        } else if !pending_failed.is_empty() {
            retry_failed(&mut pending_failed);
        }
    }
    restore(&mut ducked, &mut pending_failed);
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

/// 判断当前会话音量是否仍为我们压低后的值（期间用户手动改过或拉到 0% 静音时不动）
fn should_restore_volume(current: f32, lowered: f32) -> bool {
    if current == 0.0 && lowered > 0.005 {
        return false;
    }
    (current - lowered).abs() < 0.01
}

fn push_unique_failed(list: &mut Vec<(String, f32, f32)>, app: String, original: f32, lowered: f32) {
    if !list.iter().any(|(a, ..)| *a == app) {
        list.push((app, original, lowered));
    }
}

fn sync_failed_file(failed: &[(String, f32, f32)]) {
    if failed.is_empty() {
        let _ = std::fs::remove_file(state_path());
    } else if let Ok(text) = serde_json::to_string(failed) {
        let _ = std::fs::write(state_path(), text);
    }
}

fn restore(ducked: &mut HashMap<String, Ducked>, pending_failed: &mut Vec<(String, f32, f32)>) {
    for (_, d) in ducked.drain() {
        match unsafe { d.volume.GetMasterVolume() } {
            Ok(v) => {
                // 期间用户自己调过这个程序的音量就不动了
                if should_restore_volume(v, d.lowered)
                    && unsafe { d.volume.SetMasterVolume(d.original, std::ptr::null()) }.is_err()
                {
                    push_unique_failed(pending_failed, d.app, d.original, d.lowered);
                }
            }
            Err(_) => {
                // 对话中途拔了耳机等导致会话失效：保留条目写回 ducked.json，等重连或下次启动再恢复
                push_unique_failed(pending_failed, d.app, d.original, d.lowered);
            }
        }
    }
    sync_failed_file(pending_failed);
}

fn retry_failed(pending_failed: &mut Vec<(String, f32, f32)>) {
    if pending_failed.is_empty() {
        return;
    }
    let active = sessions();
    let before = pending_failed.len();
    pending_failed.retain(|(app, original, lowered)| {
        let mut matched = false;
        let mut any_err = false;
        for s in active.iter().filter(|s| s.app == *app) {
            matched = true;
            match unsafe { s.volume.GetMasterVolume() } {
                Ok(v) => {
                    if should_restore_volume(v, *lowered)
                        && unsafe { s.volume.SetMasterVolume(*original, std::ptr::null()) }.is_err()
                    {
                        any_err = true;
                    }
                }
                Err(_) => any_err = true,
            }
        }
        !matched || any_err
    });
    if pending_failed.len() != before {
        sync_failed_file(pending_failed);
    }
}

/// 上次调低后没来得及恢复（Aeri 被结束、崩溃或中途拔过耳机）：按程序把音量恢复回去
fn restore_leftovers() -> Vec<(String, f32, f32)> {
    let Ok(text) = std::fs::read_to_string(state_path()) else { return Vec::new() };
    let saved: Vec<(String, f32, f32)> = serde_json::from_str(&text).unwrap_or_default();
    let mut failed = Vec::new();
    let active = sessions();
    for (app, original, lowered) in saved {
        let mut any_err = false;
        for s in active.iter().filter(|s| s.app == app) {
            match unsafe { s.volume.GetMasterVolume() } {
                Ok(v) => {
                    if should_restore_volume(v, lowered)
                        && unsafe { s.volume.SetMasterVolume(original, std::ptr::null()) }.is_err()
                    {
                        any_err = true;
                    }
                }
                Err(_) => any_err = true,
            }
        }
        if any_err {
            push_unique_failed(&mut failed, app, original, lowered);
        }
    }
    sync_failed_file(&failed);
    failed
}

fn save(ducked: &HashMap<String, Ducked>, pending_failed: &[(String, f32, f32)]) {
    let mut saved: Vec<(&str, f32, f32)> = ducked.values().map(|d| (d.app.as_str(), d.original, d.lowered)).collect();
    for (app, original, lowered) in pending_failed {
        if !saved.iter().any(|(a, ..)| *a == app.as_str()) {
            saved.push((app.as_str(), *original, *lowered));
        }
    }
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

/// 所有播放设备上其他程序的音频会话（不含 Aeri 自己和系统提示音）
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

    #[test]
    fn manual_zero_mute_from_low_volume_does_not_bounce_back() {
        // 原音量 4% -> 压低到 0.8%（> 0.005）：未改动时应恢复，手动拉到 0.0 时绝不回弹
        assert!(should_restore_volume(0.008, 0.008));
        assert!(!should_restore_volume(0.0, 0.008));
        // 本来就是 0% 的不受影响；用户手动调大后也不覆盖
        assert!(should_restore_volume(0.0, 0.0));
        assert!(!should_restore_volume(0.5, 0.1));
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
