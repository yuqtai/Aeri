# 手机控制协议

电脑端：`src/remote.rs`；手机端：`app/src/main/java/com/aeri/remote/MainViewModel.kt`、`Discovery.kt`。

端口默认 47821（配置项 `remote_port`），TCP 上跑 WebSocket，UDP 上做局域网发现。电脑只接受局域网和本机地址（192.168.x、10.x、172.16–31.x、169.254.x、127.x）。

## 局域网发现（UDP）

手机向 `255.255.255.255:47821` 和各网卡所在网段的广播地址发送 `AERI_DISCOVER`，电脑回：

```json
{"t": "aeri", "pc": "电脑名", "id": "这台电脑的固定编号", "port": 47821}
```

手机用回包的来源地址作为电脑 IP。`id` 存在 `%APPDATA%\VoiceAssistant\remote.json`，不随 IP 变化；手机连不上时按 `id` 重新找，找到新 IP 后更新。

## WebSocket

`ws://<电脑 IP>:47821/`。文本帧是 JSON，二进制帧是手机麦克风的声音（16kHz、单声道、16 位小端 PCM，每帧 40 毫秒）。

### 第一条消息：认证

| 手机发 | 电脑回 |
|---|---|
| `{"t":"pair","code":"123456","name":"手机型号"}` | `{"t":"paired","token":"64 位十六进制","pc":"电脑名","id":"..."}`，接着是 `welcome` |
| `{"t":"hello","token":"..."}` | `welcome` |

失败时回 `{"t":"denied","reason":...}` 并断开：

- `no_pairing`：电脑上没在配对，或配对码过期（5 分钟）、输错 5 次作废
- `bad_code`：配对码不对
- `unpaired`：令牌无效（电脑上取消了配对）
- `save_failed`：电脑保存配对信息失败

配对码在电脑设置页点“配对新手机”时生成；令牌用 DPAPI 加密后存在 `remote.json`。

### 电脑 → 手机

```json
{"t": "welcome", "pc": "电脑名", "id": "...", "status": 0, "task_running": false, "partial": "", "history": [{"role": 1, "text": "..."}],
 "app": {"version_code": 304, "version_name": "0.3.4", "size": 1405324}}
{"t": "status", "s": 2}
{"t": "partial", "text": "正在识别的半句话"}
{"t": "chat", "item": {"role": 2, "text": "任务 #3 · 打开浏览器", "task": 3}}
{"t": "task_running", "on": true}
{"t": "clear"}
```

- `status`：0 待机、1 正在听、2 思考中、3 正在操作、4 正在回答、5 语音唤醒已暂停
- `role`：0 用户、1 助手、2 工具/任务进展、3 系统提示；带 `task` 的是后台任务进展，紧挨着的同一任务进展原地替换
- `history` 是最近 100 条；手机网络卡住、落后太多时电脑会重发一份 `welcome`
- `app` 是电脑上带的手机 App 版本（见下面的“App 更新”）；电脑上没有安装包时为 `null`

### 手机 → 电脑

```json
{"t": "text", "text": "打开计算器"}
{"t": "stop"}
{"t": "talk"}
{"t": "listen", "on": false}
{"t": "media", "key": "volume_up"}
{"t": "ptt", "on": true}
{"t": "ptt", "on": false}
{"t": "ptt", "on": false, "cancel": true}
```

- `talk`：让电脑用它自己的麦克风开始听
- `get_app`：要电脑上带的手机 App 安装包（见下面的“App 更新”）
- `media.key`：`volume_up` / `volume_down` / `mute` / `play_pause` / `next` / `previous`
- `ptt`：按住说话。按下后发二进制声音帧，松开时先发完最后一帧再发 `on:false`。按得太短（不到 0.4 秒）时带 `cancel:true`，电脑丢掉这次的声音

按下说话键时电脑的处理：

- 待机：开始一轮“听手机”（不响提示音，电脑没有麦克风也能用）；
- 正在听：声音直接接着送进当前这一轮；
- 正在思考、执行或回答：先停下，回到待机后马上开始听手机。

还没开始收音时收到的声音先存着，开始收音时按顺序补发，开头的字不会丢。松开后补 1.5 秒静音，让服务端判断这句话说完了。这一轮里电脑麦克风的声音不送去识别，唤醒词照常听。连接断开时电脑自动当作松开。

## App 更新

发布版的 `build.ps1` 会一起编译手机 App，放在 exe 旁边的 `android\Aeri.apk`，版本信息写在 `android\Aeri.json`：

```json
{"version_code": 304, "version_name": "0.3.4", "size": 1405324, "sha256": "..."}
```

App 的版本号和电脑版一致（取 `Cargo.toml`），`version_code` = 主版本 × 10000 + 次版本 × 100 + 修订号。电脑版更新时 `android\` 跟着换新，所以手机 App 不需要连外网就能更新：

1. 手机在 `welcome.app` 里看到的 `version_code` 比自己大，就提示“电脑上有新版 App”；
2. 点“更新”时先确认系统允许 Aeri 安装应用，然后发 `{"t":"get_app"}`；
3. 电脑回 `{"t":"app_begin","size":...,"sha256":"...","version_name":"..."}`，接着是若干个二进制块（每块 256 KB），最后 `{"t":"app_end"}`；电脑上没有安装包或版本信息对不上时回 `{"t":"app_error","reason":"missing"}`；
4. 手机核对大小和 SHA-256，通过后打开系统安装界面，由用户点“更新”确认。

下载过程中的二进制块是电脑发给手机的，和手机发给电脑的麦克风声音方向相反，不会混在一起。
