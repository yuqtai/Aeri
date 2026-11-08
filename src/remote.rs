//! 手机控制：同一个局域网里的安卓 App（android/）连到这里，可以打字下命令、按住说话（用手机麦克风）、
//! 停止、暂停唤醒、调音量和控制播放，并实时看到对话和状态。
//!
//! - 只接受局域网地址（192.168.x / 10.x / 172.16–31.x / 169.254.x 和本机），不经过任何服务器；
//! - 第一次在电脑设置页点“配对新手机”，手机输入 6 位配对码，换到一个长令牌（DPAPI 加密存在本机），之后都用令牌；
//! - 同一个端口的 UDP 用来让手机自动找到电脑（局域网广播），电脑 IP 变了也不用重新配对。
//!
//! 协议见 android/PROTOCOL.md：WebSocket 文本帧是 JSON，二进制帧是手机麦克风的 16kHz 单声道 16 位 PCM。

use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::runtime::Handle;
use tokio::sync::broadcast;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::{JoinHandle, JoinSet};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::assistant::{self, Command, Role, Status, UiEvent};
use crate::audio;
use crate::config;
use crate::sys::{self, MediaKey};

/// 手机找电脑时广播的内容
const DISCOVER_REQUEST: &[u8] = b"AERI_DISCOVER";
/// 配对码多久有效
const PAIRING_VALID: Duration = Duration::from_secs(300);
/// 配对码输错几次作废
const PAIRING_MAX_ATTEMPTS: u32 = 5;
/// 手机连上时补发最近多少条对话
const HISTORY_MAX: usize = 100;
/// 连上后多久内要完成握手和认证
const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
const PING_EVERY: Duration = Duration::from_secs(15);

pub struct Hooks {
    pub commands: UnboundedSender<Command>,
    /// 暂停 / 恢复语音唤醒（要同步界面上的开关和菜单，由界面线程做）
    pub set_listening: Box<dyn Fn(bool) + Send + Sync>,
    /// 手机控制的状态变了（启动、出错、配对、连接数、配对码过期），界面刷新
    pub changed: Box<dyn Fn() + Send + Sync>,
}

static HOOKS: OnceLock<Hooks> = OnceLock::new();
static RUNTIME: OnceLock<Handle> = OnceLock::new();

pub fn init(rt: Handle, hooks: Hooks) {
    let _ = RUNTIME.set(rt);
    let _ = HOOKS.set(hooks);
}

fn changed() {
    if let Some(h) = HOOKS.get() {
        (h.changed)();
    }
}

fn send_command(c: Command) {
    if let Some(h) = HOOKS.get() {
        let _ = h.commands.send(c);
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------- 对话镜像：手机连上时补发，之后实时转发 ----------

#[derive(Clone, Serialize)]
struct Item {
    role: i32,
    text: String,
    /// 后台任务进展：同一任务紧挨着的进展原地替换
    #[serde(skip_serializing_if = "Option::is_none")]
    task: Option<u32>,
}

struct Mirror {
    status: i32,
    task_running: bool,
    partial: String,
    history: VecDeque<Item>,
}

static MIRROR: Mutex<Mirror> =
    Mutex::new(Mirror { status: Status::Idle as i32, task_running: false, partial: String::new(), history: VecDeque::new() });

impl Mirror {
    fn push(&mut self, item: Item) {
        let same_task = |prev: &Item| item.task.is_some() && prev.task == item.task;
        match self.history.back_mut() {
            Some(prev) if same_task(prev) => *prev = item,
            _ => {
                self.history.push_back(item);
                if self.history.len() > HISTORY_MAX {
                    self.history.pop_front();
                }
            }
        }
    }
}

fn events() -> &'static broadcast::Sender<Arc<str>> {
    static EVENTS: OnceLock<broadcast::Sender<Arc<str>>> = OnceLock::new();
    EVENTS.get_or_init(|| broadcast::channel(512).0)
}

/// 调度中心发给界面的每个事件也抄送一份（在发出事件的线程里调用，很快）
pub fn observe(ev: &UiEvent) {
    let mut m = lock(&MIRROR);
    let msg = match ev {
        UiEvent::Status(s) => {
            m.status = *s as i32;
            // 手机按着说话键时助手停下来了：开始听手机
            if matches!(s, Status::Idle | Status::Paused) && audio::phone_wants_turn() {
                send_command(Command::PhoneTalk);
            }
            json!({"t": "status", "s": *s as i32})
        }
        UiEvent::Partial(p) => {
            if m.partial == *p {
                return;
            }
            m.partial = p.clone();
            json!({"t": "partial", "text": p})
        }
        UiEvent::Chat(role, text) => {
            let item = Item { role: *role as i32, text: text.clone(), task: None };
            m.push(item.clone());
            json!({"t": "chat", "item": item})
        }
        UiEvent::TaskProgress(id, text) => {
            let item = Item { role: Role::Tool as i32, text: format!("任务 #{id} · {text}"), task: Some(*id) };
            m.push(item.clone());
            json!({"t": "chat", "item": item})
        }
        UiEvent::TaskRunning(on) => {
            m.task_running = *on;
            json!({"t": "task_running", "on": on})
        }
        UiEvent::Mic(_) | UiEvent::TextStarted(_) => return,
    };
    let _ = events().send(msg.to_string().into());
}

/// 界面自己显示的对话（比如在电脑上打的字），也同步给手机
pub fn note_chat(role: Role, text: &str) {
    observe(&UiEvent::Chat(role, text.to_string()));
}

/// 电脑上清空了对话
pub fn clear_history() {
    let mut m = lock(&MIRROR);
    m.history.clear();
    let _ = events().send(json!({"t": "clear"}).to_string().into());
}

/// 完整快照 + 从这一刻起的事件（同一把锁里拿，不漏不重）
fn welcome() -> (String, broadcast::Receiver<Arc<str>>) {
    let m = lock(&MIRROR);
    let rx = events().subscribe();
    let history: Vec<&Item> = m.history.iter().collect();
    let msg = json!({
        "t": "welcome",
        "pc": pc_name(),
        "id": registry(|r| r.pc_id.clone()),
        "status": m.status,
        "task_running": m.task_running,
        "partial": m.partial,
        "history": history,
        // 电脑上带的手机 App 是第几版：手机比它旧就提示更新
        "app": bundled_app().map(|a| json!({"version_code": a.version_code, "version_name": a.version_name, "size": a.size})),
    });
    (msg.to_string(), rx)
}

// ---------- 手机 App 更新 ----------
//
// 发布版把 APK 和它的版本信息放在 exe 旁边的 android\（build.ps1 生成，电脑更新时一起换新）。
// 手机连上时在 welcome 里看到版本号，比自己新就提示；点更新后发 get_app，电脑按顺序回
// app_begin（大小、SHA-256）→ 若干二进制块 → app_end，手机核对后交给系统安装。

/// 每块多大
const APP_CHUNK: usize = 256 * 1024;

#[derive(Deserialize)]
struct BundledApp {
    version_code: u64,
    version_name: String,
    size: u64,
    sha256: String,
    #[serde(skip)]
    path: PathBuf,
}

fn app_dir() -> PathBuf {
    if cfg!(test) {
        return std::env::temp_dir().join(format!("aeri-app-test-{}", std::process::id()));
    }
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("android"))).unwrap_or_default()
}

/// 电脑上带的手机 App；没有（比如调试版没编译 App）或文件对不上时返回 None
fn bundled_app() -> Option<BundledApp> {
    let dir = app_dir();
    let text = std::fs::read_to_string(dir.join("Aeri.json")).ok()?;
    let mut app: BundledApp = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    app.path = dir.join("Aeri.apk");
    let size = std::fs::metadata(&app.path).ok()?.len();
    (size == app.size && app.sha256.len() == 64).then_some(app)
}

type WsSink = futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<TcpStream>, Message>;

/// 把手机 App 传给手机
async fn send_app(sink: &mut WsSink) -> anyhow::Result<()> {
    let Some(app) = bundled_app() else {
        sink.send(Message::text(json!({"t": "app_error", "reason": "missing"}).to_string())).await?;
        return Ok(());
    };
    let path = app.path.clone();
    let bytes = tokio::task::spawn_blocking(move || std::fs::read(path)).await??;
    let begin = json!({"t": "app_begin", "size": bytes.len(), "sha256": app.sha256, "version_name": app.version_name});
    sink.send(Message::text(begin.to_string())).await?;
    for chunk in bytes.chunks(APP_CHUNK) {
        sink.send(Message::Binary(chunk.to_vec().into())).await?;
    }
    sink.send(Message::text(json!({"t": "app_end"}).to_string())).await?;
    Ok(())
}

fn current_status() -> i32 {
    lock(&MIRROR).status
}

// ---------- 已配对的手机 ----------

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct StoreFile {
    /// 这台电脑的固定编号：手机靠它认出是同一台电脑（IP 变了也一样）
    pc_id: String,
    devices: Vec<StoredDevice>,
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredDevice {
    name: String,
    /// DPAPI 加密后的令牌
    token: String,
    paired_at: u64,
}

struct Registry {
    pc_id: String,
    /// (名称, 令牌明文, 文件里的记录)
    devices: Vec<(String, String, StoredDevice)>,
}

fn store_path() -> PathBuf {
    if cfg!(test) {
        // 测试不碰真实的配对记录
        return std::env::temp_dir().join(format!("aeri-remote-test-{}.json", std::process::id()));
    }
    config::app_dir().join("remote.json")
}

impl Registry {
    fn load() -> Self {
        let file: StoreFile =
            std::fs::read_to_string(store_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let devices = file
            .devices
            .into_iter()
            .filter_map(|d| config::unprotect_key(&d.token).map(|plain| (d.name.clone(), plain, d)))
            .collect();
        let mut r = Self { pc_id: file.pc_id, devices };
        if r.pc_id.is_empty() {
            r.pc_id = uuid::Uuid::new_v4().simple().to_string();
            r.save();
        }
        r
    }

    fn save(&self) {
        let file = StoreFile { pc_id: self.pc_id.clone(), devices: self.devices.iter().map(|d| d.2.clone()).collect() };
        if let Ok(text) = serde_json::to_string_pretty(&file) {
            let _ = std::fs::create_dir_all(config::app_dir());
            let _ = std::fs::write(store_path(), text);
        }
    }

    fn add(&mut self, name: &str, token: &str) -> bool {
        let Some(protected) = config::protect_key(token) else { return false };
        let paired_at = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let stored = StoredDevice { name: name.to_string(), token: protected, paired_at };
        self.devices.push((name.to_string(), token.to_string(), stored));
        self.save();
        true
    }

    /// 令牌对得上就返回手机名
    fn check(&self, token: &str) -> Option<String> {
        self.devices.iter().find(|d| !token.is_empty() && same(&d.1, token)).map(|d| d.0.clone())
    }
}

/// 比较令牌：耗时只和长度有关，不因为前几位对上了就更慢
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn registry<T>(f: impl FnOnce(&mut Registry) -> T) -> T {
    static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);
    let mut g = lock(&REGISTRY);
    f(g.get_or_insert_with(Registry::load))
}

fn new_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

fn pc_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "电脑".into())
}

// ---------- 配对 ----------

struct Pairing {
    code: String,
    until: Instant,
    attempts: u32,
}

static PAIRING: Mutex<Option<Pairing>> = Mutex::new(None);

/// 开始配对新手机：返回 6 位配对码（5 分钟内有效）。手机控制没在运行时返回 None
pub fn start_pairing() -> Option<String> {
    if !is_running() {
        return None;
    }
    let code = format!("{:06}", uuid::Uuid::new_v4().as_u128() % 1_000_000);
    *lock(&PAIRING) = Some(Pairing { code: code.clone(), until: Instant::now() + PAIRING_VALID, attempts: 0 });
    if let Some(rt) = RUNTIME.get() {
        let expiring = code.clone();
        rt.spawn(async move {
            tokio::time::sleep(PAIRING_VALID).await;
            let mut p = lock(&PAIRING);
            if p.as_ref().is_some_and(|p| p.code == expiring) {
                *p = None;
                drop(p);
                changed();
            }
        });
    }
    changed();
    Some(code)
}

pub fn cancel_pairing() {
    if lock(&PAIRING).take().is_some() {
        changed();
    }
}

/// 手机发来配对码：对了就发一个新令牌
fn pair(code: &str, name: &str) -> Result<String, &'static str> {
    let mut p = lock(&PAIRING);
    let Some(current) = p.as_mut() else { return Err("no_pairing") };
    if Instant::now() > current.until {
        *p = None;
        return Err("no_pairing");
    }
    if !same(&current.code, code.trim()) {
        current.attempts += 1;
        if current.attempts >= PAIRING_MAX_ATTEMPTS {
            *p = None;
            drop(p);
            changed();
            return Err("no_pairing");
        }
        return Err("bad_code");
    }
    *p = None;
    drop(p);
    let name: String = name.trim().chars().take(40).collect();
    let name = if name.is_empty() { "手机".to_string() } else { name };
    let token = new_token();
    if !registry(|r| r.add(&name, &token)) {
        return Err("save_failed");
    }
    changed();
    Ok(token)
}

/// 取消所有手机的配对，已连着的也断开
pub fn unpair_all() {
    registry(|r| {
        r.devices.clear();
        r.save();
    });
    restart();
}

// ---------- 服务 ----------

struct Running {
    port: u16,
    task: JoinHandle<()>,
}

/// 服务的状态：Off ─apply(开)→ Starting ─端口打开→ Listening
///                                    └─打不开→ Failed(原因)
/// 每次 start() 都从 Starting 重新开始（端口变了、取消配对要断开所有连接时也会重启）
#[derive(Clone, Debug, PartialEq)]
enum Phase {
    Off,
    Starting,
    Listening,
    Failed(String),
}

static SERVER: Mutex<Option<Running>> = Mutex::new(None);
static PHASE: Mutex<(u64, Phase)> = Mutex::new((0, Phase::Off));
static CONNECTED: AtomicUsize = AtomicUsize::new(0);

fn is_running() -> bool {
    lock(&PHASE).1 == Phase::Listening
}

/// 只有当前这一代服务能改状态
fn set_phase(generation: u64, phase: Phase) {
    let mut p = lock(&PHASE);
    if p.0 == generation {
        p.1 = phase;
    }
}

/// 按配置打开 / 关闭手机控制（启动时和保存设置后调用；配置没变时什么也不做）
pub fn apply(enabled: bool, port: u16) {
    {
        let server = lock(&SERVER);
        let same = server.as_ref().is_some_and(|s| s.port == port && !s.task.is_finished());
        if enabled == server.is_some() && (!enabled || same) {
            return;
        }
    }
    start(enabled, port);
}

/// 重启服务，断开所有连接
fn restart() {
    let current = lock(&SERVER).as_ref().map(|s| s.port);
    if let Some(port) = current {
        start(true, port);
    } else {
        changed();
    }
}

fn start(enabled: bool, port: u16) {
    let Some(rt) = RUNTIME.get() else { return };
    let mut server = lock(&SERVER);
    if let Some(old) = server.take() {
        old.task.abort();
    }
    let generation = {
        let mut p = lock(&PHASE);
        p.0 += 1;
        p.1 = if enabled { Phase::Starting } else { Phase::Off };
        p.0
    };
    if enabled {
        let task = rt.spawn(async move {
            if let Err(e) = serve(port, generation).await {
                set_phase(generation, Phase::Failed(format!("{e:#}")));
                changed();
            }
        });
        *server = Some(Running { port, task });
    } else {
        *lock(&PAIRING) = None;
    }
    drop(server);
    changed();
}

async fn serve(port: u16, generation: u64) -> anyhow::Result<()> {
    let addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, port));
    // 刚停掉的旧服务可能还没放开端口，稍等重试几次
    let mut tries = 0;
    let (tcp, udp) = loop {
        match (TcpListener::bind(addr).await, UdpSocket::bind(addr).await) {
            (Ok(t), Ok(u)) => break (t, u),
            (Err(e), _) | (_, Err(e)) if tries >= 10 => anyhow::bail!("端口 {port} 打不开：{e}"),
            _ => {
                tries += 1;
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    };
    set_phase(generation, Phase::Listening);
    changed();
    let mut connections = JoinSet::new();
    let mut buf = [0u8; 64];
    loop {
        tokio::select! {
            accepted = tcp.accept() => {
                let Ok((stream, peer)) = accepted else { continue };
                if !is_lan(peer.ip()) {
                    continue;
                }
                connections.spawn(async move {
                    let _ = connection(stream).await;
                });
            }
            received = udp.recv_from(&mut buf) => {
                // Windows 上对方端口不可达时这里会报错，忽略即可
                if let Ok((n, peer)) = received {
                    if is_lan(peer.ip()) && &buf[..n] == DISCOVER_REQUEST {
                        let reply = json!({"t": "aeri", "pc": pc_name(), "id": registry(|r| r.pc_id.clone()), "port": port});
                        let _ = udp.send_to(reply.to_string().as_bytes(), peer).await;
                    }
                }
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

/// 只接受局域网和本机
fn is_lan(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local() || v4.is_loopback(),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_lan(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            v6.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

/// 连着的手机数（离开时减掉）
struct ConnectedGuard;

impl ConnectedGuard {
    fn new() -> Self {
        CONNECTED.fetch_add(1, Ordering::SeqCst);
        changed();
        Self
    }
}

impl Drop for ConnectedGuard {
    fn drop(&mut self) {
        CONNECTED.fetch_sub(1, Ordering::SeqCst);
        changed();
    }
}

/// 这个连接正按着说话键；连接断开（包括被强行结束）时自动松开
struct PttGuard {
    held: bool,
}

impl PttGuard {
    fn press(&mut self) {
        self.held = true;
        audio::phone_press();
        let status = current_status();
        if status == Status::Idle as i32 || status == Status::Paused as i32 {
            if audio::phone_wants_turn() {
                send_command(Command::PhoneTalk);
            }
        } else if status != Status::Listening as i32 {
            // 正在思考、执行或回答：先停下，回到待机时（observe 里）开始听手机
            assistant::interrupt();
        }
        // 正在听：声音直接接着送过去
    }

    fn release(&mut self) {
        if std::mem::take(&mut self.held) {
            audio::phone_release();
        }
    }

    /// 按一下就松开了：不当成说话
    fn cancel(&mut self) {
        self.held = false;
        if audio::phone_cancel() {
            assistant::interrupt();
        }
    }
}

impl Drop for PttGuard {
    fn drop(&mut self) {
        self.release();
    }
}

async fn connection(stream: TcpStream) -> anyhow::Result<()> {
    let _ = stream.set_nodelay(true);
    let config = WebSocketConfig::default().max_message_size(Some(1 << 20)).max_frame_size(Some(1 << 20));
    let ws = tokio::time::timeout(AUTH_TIMEOUT, tokio_tungstenite::accept_async_with_config(stream, Some(config))).await??;
    let (mut sink, mut source) = ws.split();

    // 第一条消息：已配对的手机发令牌，新手机发配对码
    let first = tokio::time::timeout(AUTH_TIMEOUT, async {
        loop {
            match source.next().await {
                Some(Ok(Message::Text(t))) => return serde_json::from_str::<Value>(t.as_str()).ok(),
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                _ => return None,
            }
        }
    })
    .await?
    .unwrap_or_default();
    let denied = |reason: &str| Message::text(json!({"t": "denied", "reason": reason}).to_string());
    match first["t"].as_str() {
        Some("hello") => {
            if registry(|r| r.check(first["token"].as_str().unwrap_or(""))).is_none() {
                sink.send(denied("unpaired")).await?;
                return Ok(());
            }
        }
        Some("pair") => {
            match pair(first["code"].as_str().unwrap_or(""), first["name"].as_str().unwrap_or("")) {
                Ok(token) => {
                    let reply = json!({"t": "paired", "token": token, "pc": pc_name(), "id": registry(|r| r.pc_id.clone())});
                    sink.send(Message::text(reply.to_string())).await?;
                }
                Err(reason) => {
                    sink.send(denied(reason)).await?;
                    return Ok(());
                }
            }
        }
        _ => return Ok(()),
    }

    let (snapshot, mut events) = welcome();
    sink.send(Message::text(snapshot)).await?;
    let _connected = ConnectedGuard::new();
    let mut ptt = PttGuard { held: false };
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    loop {
        tokio::select! {
            ev = events.recv() => match ev {
                Ok(msg) => sink.send(Message::text(msg.to_string())).await?,
                // 落后太多（网络卡了）：重发一份完整快照
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let (snapshot, rx) = welcome();
                    events = rx;
                    sink.send(Message::text(snapshot)).await?;
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            msg = source.next() => match msg {
                Some(Ok(Message::Text(t))) => {
                    if let Ok(v) = serde_json::from_str::<Value>(t.as_str()) {
                        if handle_message(&v, &mut ptt) == Some(Reply::SendApp) {
                            send_app(&mut sink).await?;
                        }
                    }
                }
                Some(Ok(Message::Binary(pcm))) => {
                    if ptt.held {
                        audio::phone_audio(pcm16_to_f32(&pcm));
                    }
                }
                Some(Ok(Message::Close(_))) | None => return Ok(()),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
            },
            _ = ping.tick() => sink.send(Message::Ping(Vec::new().into())).await?,
        }
    }
}

/// 要回给这台手机的东西（其余命令不用回，结果通过事件同步）
#[derive(PartialEq, Debug)]
enum Reply {
    SendApp,
}

fn handle_message(v: &Value, ptt: &mut PttGuard) -> Option<Reply> {
    match v["t"].as_str().unwrap_or("") {
        "get_app" => return Some(Reply::SendApp),
        "text" => {
            let text = v["text"].as_str().unwrap_or("").trim();
            if !text.is_empty() {
                send_command(Command::Text(text.chars().take(2000).collect()));
            }
        }
        "stop" => assistant::interrupt(),
        // 用电脑的麦克风听
        "talk" => send_command(Command::Talk),
        "ptt" => match (v["on"].as_bool(), v["cancel"].as_bool()) {
            (Some(true), _) => ptt.press(),
            (_, Some(true)) => ptt.cancel(),
            _ => ptt.release(),
        },
        "listen" => {
            if let (Some(on), Some(h)) = (v["on"].as_bool(), HOOKS.get()) {
                (h.set_listening)(on);
            }
        }
        "media" => {
            let key = match v["key"].as_str().unwrap_or("") {
                "volume_up" => MediaKey::VolumeUp,
                "volume_down" => MediaKey::VolumeDown,
                "mute" => MediaKey::Mute,
                "play_pause" => MediaKey::PlayPause,
                "next" => MediaKey::Next,
                "previous" => MediaKey::Previous,
                _ => return None,
            };
            let times = v["times"].as_u64().unwrap_or(1).clamp(1, 10) as u32;
            tokio::task::spawn_blocking(move || sys::press_media_key(key, times));
        }
        _ => {}
    }
    None
}

fn pcm16_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect()
}

// ---------- 给设置页显示 ----------

pub struct Info {
    pub running: bool,
    /// 正在配对时的配对码
    pub code: String,
    pub paired: usize,
    /// 一行说明：地址、已配对几台、几台在线，或出错原因
    pub status: String,
}

pub fn info() -> Info {
    let phase = lock(&PHASE).1.clone();
    let running = phase == Phase::Listening;
    let error = if let Phase::Failed(e) = &phase { e.clone() } else { String::new() };
    let paired = registry(|r| r.devices.len());
    let connected = CONNECTED.load(Ordering::SeqCst);
    let port = lock(&SERVER).as_ref().map(|s| s.port);
    let status = if !error.is_empty() {
        format!("没能打开：{error}")
    } else if running {
        let address = local_ip().map(|ip| format!("电脑地址 {ip}:{}", port.unwrap_or_default())).unwrap_or_default();
        let devices = match (paired, connected) {
            (0, _) => "还没有配对手机".to_string(),
            (n, 0) => format!("已配对 {n} 台手机"),
            (n, c) => format!("已配对 {n} 台手机，{c} 台在线"),
        };
        [address, devices].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")
    } else if port.is_some() {
        "正在打开…".into()
    } else {
        "打开开关并保存后，就可以配对手机".into()
    };
    Info { running, code: lock(&PAIRING).as_ref().map(|p| p.code.clone()).unwrap_or_default(), paired, status }
}

/// 本机在局域网里的地址（只用来显示；手机靠广播自动找到电脑）
fn local_ip() -> Option<Ipv4Addr> {
    // UDP 的 connect 不发任何包，只是让系统选出走哪块网卡
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.168.0.1:9").ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if ip.is_private() || ip.is_link_local() => Some(ip),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_lan_addresses_are_accepted() {
        for ok in ["192.168.1.5", "10.0.0.2", "172.20.1.1", "169.254.3.4", "127.0.0.1", "::1", "fe80::1", "fd00::5", "::ffff:192.168.1.9"] {
            assert!(is_lan(ok.parse().unwrap()), "{ok}");
        }
        for bad in ["8.8.8.8", "100.64.0.1", "172.32.0.1", "2001:db8::1", "::ffff:1.1.1.1"] {
            assert!(!is_lan(bad.parse().unwrap()), "{bad}");
        }
    }

    #[test]
    fn pcm_is_converted_little_endian() {
        let bytes = [0x00, 0x40, 0x00, 0xc0, 0xff, 0x7f];
        let s = pcm16_to_f32(&bytes);
        assert_eq!(s.len(), 3);
        assert!((s[0] - 0.5).abs() < 1e-6 && (s[1] + 0.5).abs() < 1e-6 && s[2] > 0.99);
    }

    #[test]
    fn token_compare() {
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd"));
        assert!(!same("abc", "abcd"));
    }

    /// 模拟一台手机走完整个流程：广播找电脑 → 配对 → 用令牌登录 → 收对话 → 打字 → 按住说话 → 取消配对
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn phone_end_to_end() {
        use tokio::sync::mpsc::unbounded_channel;
        let _serial = audio::PHONE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use tokio_tungstenite::connect_async;

        // 电脑上带的手机 App（假的，比一块大一点，验证分块）
        let apk: Vec<u8> = (0..APP_CHUNK + 1000).map(|i| (i * 31 % 251) as u8).collect();
        let dir = app_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Aeri.apk"), &apk).unwrap();
        let fake_sha = "ab".repeat(32);
        let manifest = json!({"version_code": 305, "version_name": "0.3.5", "size": apk.len(), "sha256": fake_sha});
        // 带 BOM 也要能读（Windows PowerShell 5 写的文件）
        std::fs::write(dir.join("Aeri.json"), format!("\u{feff}{manifest}")).unwrap();

        let (cmd_tx, mut cmd_rx) = unbounded_channel();
        let (listen_tx, mut listen_rx) = unbounded_channel();
        init(
            Handle::current(),
            Hooks {
                commands: cmd_tx,
                set_listening: Box::new(move |on| {
                    let _ = listen_tx.send(on);
                }),
                changed: Box::new(|| {}),
            },
        );
        let (audio_tx, mut audio_rx) = unbounded_channel();
        audio::set_phone_output(audio_tx);
        audio::set_mode(audio::Mode::Idle);

        let port = {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            probe.local_addr().unwrap().port()
        };
        apply(true, port);
        for _ in 0..50 {
            if is_running() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(is_running(), "{}", info().status);
        let url = format!("ws://127.0.0.1:{port}/");
        let wait = Duration::from_secs(3);

        // 局域网发现
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        udp.send_to(DISCOVER_REQUEST, ("127.0.0.1", port)).await.unwrap();
        let mut buf = [0u8; 512];
        let (n, _) = tokio::time::timeout(wait, udp.recv_from(&mut buf)).await.unwrap().unwrap();
        let reply: Value = serde_json::from_slice(&buf[..n]).unwrap();
        assert_eq!(reply["t"], "aeri");
        assert_eq!(reply["port"], port);
        let pc_id = reply["id"].as_str().unwrap().to_string();

        async fn next_json<S>(ws: &mut S) -> Value
        where
            S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
        {
            loop {
                match tokio::time::timeout(Duration::from_secs(3), ws.next()).await.expect("等消息超时") {
                    Some(Ok(Message::Text(t))) => return serde_json::from_str(t.as_str()).unwrap(),
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                    other => panic!("意外的消息：{other:?}"),
                }
            }
        }
        let send = |v: Value| Message::text(v.to_string());

        // 没在配对时发配对码：拒绝
        let (mut ws, _) = connect_async(url.as_str()).await.unwrap();
        ws.send(send(json!({"t": "pair", "code": "000000", "name": "测试手机"}))).await.unwrap();
        assert_eq!(next_json(&mut ws).await["reason"], "no_pairing");

        // 配对码输错：拒绝；输对：拿到令牌，紧接着是完整快照
        let code = start_pairing().unwrap();
        assert_eq!(info().code, code);
        let wrong = if code == "111111" { "222222" } else { "111111" };
        let (mut ws, _) = connect_async(url.as_str()).await.unwrap();
        ws.send(send(json!({"t": "pair", "code": wrong, "name": "测试手机"}))).await.unwrap();
        assert_eq!(next_json(&mut ws).await["reason"], "bad_code");
        observe(&UiEvent::Chat(Role::User, "配对前说的话".into()));
        let (mut ws, _) = connect_async(url.as_str()).await.unwrap();
        ws.send(send(json!({"t": "pair", "code": code, "name": "测试手机"}))).await.unwrap();
        let paired = next_json(&mut ws).await;
        assert_eq!(paired["t"], "paired");
        assert_eq!(paired["id"], pc_id.as_str());
        let token = paired["token"].as_str().unwrap().to_string();
        assert_eq!(token.len(), 64);
        let welcome = next_json(&mut ws).await;
        assert_eq!(welcome["t"], "welcome");
        assert!(welcome["history"].as_array().unwrap().iter().any(|i| i["text"] == "配对前说的话"));
        assert_eq!(info().code, "", "配对成功后配对码作废");
        drop(ws);

        // 令牌不对：拒绝
        let (mut ws, _) = connect_async(url.as_str()).await.unwrap();
        ws.send(send(json!({"t": "hello", "token": "x".repeat(64)}))).await.unwrap();
        assert_eq!(next_json(&mut ws).await["reason"], "unpaired");

        // 用令牌登录；快照里带着电脑上的 App 版本
        let (mut ws, _) = connect_async(url.as_str()).await.unwrap();
        ws.send(send(json!({"t": "hello", "token": token}))).await.unwrap();
        let welcome = next_json(&mut ws).await;
        assert_eq!(welcome["t"], "welcome");
        assert_eq!(welcome["app"], json!({"version_code": 305, "version_name": "0.3.5", "size": apk.len()}));

        // 手机要 App：开头说明大小和校验值，然后按顺序分块，最后结束
        ws.send(send(json!({"t": "get_app"}))).await.unwrap();
        let begin = next_json(&mut ws).await;
        assert_eq!((begin["t"].as_str(), begin["size"].as_u64()), (Some("app_begin"), Some(apk.len() as u64)));
        assert_eq!(begin["sha256"], fake_sha.as_str());
        let mut got = Vec::new();
        let mut chunks = 0;
        loop {
            match tokio::time::timeout(wait, ws.next()).await.unwrap() {
                Some(Ok(Message::Binary(b))) => {
                    got.extend_from_slice(&b);
                    chunks += 1;
                }
                Some(Ok(Message::Text(t))) => {
                    assert_eq!(serde_json::from_str::<Value>(t.as_str()).unwrap()["t"], "app_end");
                    break;
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                other => panic!("意外的消息：{other:?}"),
            }
        }
        assert_eq!(chunks, 2);
        assert!(got == apk, "收到的 App 和原文件不一样");
        // App 和版本信息对不上（比如只换了 APK 没换版本信息）：不提示更新
        std::fs::write(dir.join("Aeri.apk"), b"short").unwrap();
        assert!(bundled_app().is_none());
        ws.send(send(json!({"t": "get_app"}))).await.unwrap();
        assert_eq!(next_json(&mut ws).await["t"], "app_error");

        // 电脑上的事件实时转发；同一任务的进展带着编号
        observe(&UiEvent::Status(Status::Thinking));
        assert_eq!(next_json(&mut ws).await, json!({"t": "status", "s": 2}));
        observe(&UiEvent::TaskProgress(7, "打开浏览器".into()));
        let progress = next_json(&mut ws).await;
        assert_eq!(progress["item"]["task"], 7);
        assert_eq!(progress["item"]["text"], "任务 #7 · 打开浏览器");

        // 打字、暂停唤醒
        ws.send(send(json!({"t": "text", "text": "  打开计算器 "}))).await.unwrap();
        match tokio::time::timeout(wait, cmd_rx.recv()).await.unwrap() {
            Some(Command::Text(t)) => assert_eq!(t, "打开计算器"),
            _ => panic!("应该收到文字命令"),
        }
        ws.send(send(json!({"t": "listen", "on": false}))).await.unwrap();
        assert_eq!(tokio::time::timeout(wait, listen_rx.recv()).await.unwrap(), Some(false));

        // 按住说话：待机时请调度中心开始听手机；开始前收到的声音先存着，开始时按顺序补发，松开时补静音
        observe(&UiEvent::Status(Status::Idle));
        assert_eq!(next_json(&mut ws).await["s"], 0);
        ws.send(send(json!({"t": "ptt", "on": true}))).await.unwrap();
        assert!(matches!(tokio::time::timeout(wait, cmd_rx.recv()).await.unwrap(), Some(Command::PhoneTalk)));
        let pcm: Vec<u8> = (0..640i16).flat_map(|i| (i * 10).to_le_bytes()).collect();
        ws.send(Message::Binary(pcm.clone().into())).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(audio_rx.try_recv().is_err(), "还没开始听，声音应该先存着");
        assert!(audio::phone_begin_turn());
        let first = tokio::time::timeout(wait, audio_rx.recv()).await.unwrap().unwrap();
        assert_eq!(first, pcm16_to_f32(&pcm));
        ws.send(Message::Binary(pcm.clone().into())).await.unwrap();
        assert_eq!(tokio::time::timeout(wait, audio_rx.recv()).await.unwrap().unwrap().len(), 640);
        ws.send(send(json!({"t": "ptt", "on": false}))).await.unwrap();
        let silence = tokio::time::timeout(wait, audio_rx.recv()).await.unwrap().unwrap();
        assert!(silence.len() >= 16000 && silence.iter().all(|s| *s == 0.0));
        audio::set_mode(audio::Mode::Idle);
        audio::phone_end_turn();
        assert!(!audio::phone_active());

        // 媒体键用未知键名：忽略，连接不受影响
        ws.send(send(json!({"t": "media", "key": "nope"}))).await.unwrap();

        // 取消全部配对：连着的手机被断开，旧令牌失效
        unpair_all();
        let closed = tokio::time::timeout(wait, async {
            loop {
                match ws.next().await {
                    Some(Ok(Message::Text(_) | Message::Ping(_) | Message::Pong(_))) => continue,
                    _ => return,
                }
            }
        })
        .await;
        assert!(closed.is_ok(), "取消配对后应该断开");
        for _ in 0..50 {
            if is_running() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let (mut ws, _) = connect_async(url.as_str()).await.unwrap();
        ws.send(send(json!({"t": "hello", "token": token}))).await.unwrap();
        assert_eq!(next_json(&mut ws).await["reason"], "unpaired");

        apply(false, port);
        let _ = std::fs::remove_file(store_path());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn task_progress_replaces_in_place() {
        let mut m = Mirror { status: 0, task_running: false, partial: String::new(), history: VecDeque::new() };
        m.push(Item { role: 2, text: "任务 #1 · a".into(), task: Some(1) });
        m.push(Item { role: 2, text: "任务 #1 · b".into(), task: Some(1) });
        m.push(Item { role: 1, text: "好".into(), task: None });
        m.push(Item { role: 2, text: "任务 #1 · c".into(), task: Some(1) });
        let texts: Vec<&str> = m.history.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, ["任务 #1 · b", "好", "任务 #1 · c"]);
    }
}
