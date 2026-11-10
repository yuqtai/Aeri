# 开发、编译与发版

## 架构

```text
麦克风 ─► 降采样 16k ─► 能量门限 ─► 唤醒词模型（sherpa-onnx KWS，本地）
                                              │ 检测到唤醒词
                                              ▼
 前台（二选一，设置里切换）
   实时语音（默认）：音频发给百炼 Qwen-Audio Realtime 断句、转写 → DeepSeek 想、调工具 → 实时语音读出来（24kHz）
   识别 + 大模型：fun-asr 识别一句话 → 本地直达规则 / DeepSeek → Windows SAPI 播报
                                              │ 前台工具
          ┌───────────────────────────────────┴────────────────────────┐
   自己做（快）：启动/关闭程序、网站搜索、                    start_task / task_status / cancel_task
   时间、音量、桌面控制、浏览器控制                                     │ 只有费时的才交出去（不阻塞对话）
                                                                      ▼
                                        Codex（codex exec）/ Claude Code（claude -p）/ 内置 DeepSeek
                                        └─ 通过 MCP 使用本程序的工具（aeri.exe --mcp）：
                                           启动/关闭程序、agent-desktop 读控件/点击/填写/勾选/快捷键、读屏、浏览器控制
                                                                      │ 做完
                                                                      ▼
                                                        前台主动播报结果，可以接着追问
```

播报期间：耳朵只跑唤醒词，说唤醒词就停下播报开始听（外放时不把麦克风发给实时模型，避免它听到自己）；戴耳机时可以打开“直接插话”，播报时麦克风一直开着。

对话期间默认把其他程序的声音（视频、音乐）调到原来的 20%，结束后恢复，免得麦克风把电脑里的声音也当成用户在说话；Aeri 自己的播报不受影响。期间手动调过音量的程序不会被改回去；Aeri 中途退出时，下次启动会先把音量恢复。可以在设置里关掉。

| 文件 | 作用 |
|---|---|
| `src/main.rs` | 界面、托盘、把界面事件接到调度中心；`--kws-test` 测试唤醒词；`--mcp` 给后台智能体提供桌面工具 |
| `src/ball.rs` | 悬浮球：Win32 分层窗口，逐像素自绘（Slint 窗口做不了透明圆形） |
| `src/sys.rs` | 单实例、内存统计、媒体键、开机自启（注册表启动项）等系统小工具 |
| `src/assistant.rs` | 调度中心：唤醒 → 前台对话（实时语音 / 识别+大模型）→ 播报和打断 → 后台任务完成后主动播报 |
| `src/tasks.rs` | 后台任务：启动 Codex / Claude Code / 内置助手，解析它们的事件流显示进度，取消时结束整个进程树 |
| `src/realtime.rs` / `src/player.rs` | 百炼实时语音模型的 WebSocket 客户端；24kHz 语音播放（可随时清空） |
| `src/mcp.rs` | MCP 服务（标准输入输出 JSON-RPC），把桌面工具提供给后台智能体 |
| `src/audio.rs` | 麦克风采集、能量门限、耳朵线程（待机/录音/静音/暂停/播报中听唤醒词） |
| `src/kws.rs` | 唤醒词检测，中文唤醒词自动转拼音 token，可随时修改 |
| `src/asr.rs` | fun-asr-realtime WebSocket 客户端 |
| `src/llm.rs` / `src/agent.rs` | DeepSeek（Anthropic 兼容接口）和工具调用循环，分前台（对话）和后台（内置执行者）两种 |
| `src/fastpath.rs` | 不经过大模型的常用命令（打开/关闭程序、“打开抖音搜索 X”这类网站搜索、时间、音量） |
| `src/tools/` | 本地工具、文件操作（`files.rs`）、系统工具（`system.rs`：音量、系统开关、截图、电源、窗口、系统状态、计算、剪贴板历史）、关闭程序（含后台进程）、桌面控制和剪贴板（`desktop.rs`，调用 agent-desktop）、读屏（`reader.rs`，直接用 UI Automation）、按需启动的浏览器控制 |
| `src/update.rs` | 软件更新：检查 GitHub Release、下载校验、替换文件（失败回滚）并重启 |
| `src/reminders.rs` | 提醒和倒计时，存在 `reminders.json`，重启后还在，到点由助手主动说出来 |
| `src/remote.rs` | 手机控制：局域网 WebSocket 服务、UDP 局域网发现、配对码和令牌、把对话和状态同步给手机 |
| `src/screen.rs` | 屏幕上下文：用户正在看的窗口（标题、程序、是否全屏），每句话都带给大模型；也负责按标题快速找窗口 |
| `src/tts.rs` | SAPI 播报，可中途打断 |
| `ui/app.slint` | 界面（Slint，软件渲染，不占 GPU） |
| `android/` | 安卓 App（Kotlin + Jetpack Compose）：找电脑、配对、按住说话、打字、快捷控制；协议见 `android/PROTOCOL.md` |

## 编译和运行

需要 Rust（`rustup`）和 VS 2022 生成工具（C++）。下面的命令在 PowerShell 或 cmd 里都能直接粘贴运行，不用先切换目录（路径按本机的 `D:\code\voice-assistant` 写，放在别处请相应替换）。

0. 第一次编译前下载唤醒词模型（仓库里不带，约 31 MB）：从 sherpa-onnx 的 [kws-models 发布页](https://github.com/k2-fsa/sherpa-onnx/releases/tag/kws-models) 下载 `sherpa-onnx-kws-zipformer-zh-en-3M-2025-12-20.tar.bz2`，解压后把里面的文件放到项目的 `models\kws\` 目录。

1. 编译发布版（第一次要几分钟）：

   ```powershell
   powershell -ExecutionPolicy Bypass -File D:\code\voice-assistant\build.ps1 -Release
   ```

2. 编译好的程序在 `dist\` 里（exe、agent-desktop.exe、4 个 DLL 和 `models\` 都已放好），双击 `dist\aeri.exe` 就能用，或者运行：

   ```powershell
   D:\code\voice-assistant\dist\aeri.exe
   ```

3. 想开机自动启动：在设置页打开“开机自动启动”，点“保存并应用”。

说明：

- 以后重新编译会自动更新 `dist\`；如果助手正在运行，脚本会先把它关掉。编译的中间文件放在用户目录下的 `.cargo-target\va`，避开 Windows 260 字符的路径限制，不用管它。
- 发布版没有控制台窗口，启动后桌面右下角出现悬浮球，托盘里有蓝色麦克风图标；关闭主界面只是缩回悬浮球，右键悬浮球或托盘图标选“退出”才真正退出。
- 开机自启写的是当前用户的注册表启动项（`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`，不需要管理员权限），登记的是正在运行的 exe 的路径，所以请从 `dist\` 里的 exe 打开再设置；之后如果把程序挪了位置，手动启动一次就会自动更新路径。开机自启时只显示悬浮球，不弹主界面。
- 分发时把整个 `dist` 文件夹拷走即可。
- 桌面控制组件 [agent-desktop](https://github.com/vercel-labs/agent-desktop)（Apache-2.0）不放在仓库里：第一次编译时 `build.ps1` 从它的 GitHub 发布页下载固定版本（v1.0.2，Windows x64），校验 SHA-256 后缓存在 `vendor\`，再复制到 exe 旁边。升级时改 `build.ps1` 里的版本号和校验值。
- 浏览器控制需要 Node.js（第一次使用时通过 npx 下载 Playwright MCP）。
- 测试唤醒词：`aeri.exe --kws-test a.wav b.wav`。
- 运行单元测试：`powershell -ExecutionPolicy Bypass -File D:\code\voice-assistant\build.ps1 -Test`。

编译安卓 App：需要 JDK 17 以上和安卓 SDK（`ANDROID_HOME` 指向 SDK，或在 `android\local.properties` 里写 `sdk.dir`），第一次会下载 Gradle 依赖：

```powershell
cd D:\code\voice-assistant\android; .\gradlew.bat assembleRelease
```

生成的 APK 在 `android\app\build\outputs\apk\release\app-release.apk`。

- `build.ps1 -Release` 会顺带编译手机 App，放到 `dist\android\Aeri.apk`，并写一份版本信息 `Aeri.json`（版本号、大小、SHA-256）；电脑把它传给配对过的手机更新（见 [android/PROTOCOL.md](../android/PROTOCOL.md) 的“App 更新”）。调试版要带上 App 时加 `-Android`。没有安卓 SDK 或 JDK 时只给警告、跳过 App。
- App 的版本号取自 `Cargo.toml`，不用单独改；`release.ps1` 发版时把 App 打进 zip，另外单独上传一份 `Aeri-vX.Y.Z-android.apk` 方便第一次安装。
- App 用本机的安卓调试证书签名（`%USERPROFILE%\.android\debug.keystore`）。**换电脑发版前把它拷过去**：签名不一样时手机上没法直接更新，只能先卸载再装（配对要重做）。

## 安装与更新

首次运行 `powershell -ExecutionPolicy Bypass -File D:\code\voice-assistant\install.ps1 -Launch`，安装到当前用户的 `%LOCALAPPDATA%\Programs\VoiceAssistant`，不需要管理员权限。桌面和开始菜单会出现“Aeri”，Windows“已安装的应用”中可以卸载。设置和 API Key 仍保存在原位置，更新及卸载均保留。

安装后，每次 `build.ps1 -Release` 会自动更新并启动安装版。更新时会停止旧版本，请先等正在执行的任务结束。也可以单独重新运行 `install.ps1` 更新已有发布文件。
**软件内更新**：设置页底部“软件更新”可以检查并一键更新；默认每天自动检查一次，有新版本时在对话里提醒（不会自己安装），也可以直接说“更新软件”。也可以在命令行运行 `aeri.exe --update`。更新流程：从 GitHub Release 下载 `Aeri-vX.Y.Z-windows-x64.zip`，按 GitHub 给出的 SHA-256 校验，解压到 `%TEMP%\aeri-update`，由新版本的 `aeri.exe --apply-update` 等旧程序退出后替换安装目录里的文件并重新启动；替换失败会恢复原来的文件。设置、API Key、提醒在 `%APPDATA%\VoiceAssistant`，不受影响。

**发版**：改好 `Cargo.toml` 里的版本号并提交后运行 `.\scripts\release.ps1 -Notes 说明.md`，会编译、打包、生成校验文件、推送标签并创建 GitHub Release。自动更新依赖附件的文件名，不要改。

## 应用图标

品牌源文件为 `assets/app.svg`，多尺寸 Windows 图标为 `assets/app.ico`；窗口、托盘与 exe 共用同一图形。运行 `python scripts/generate_icon.py`（需要 Pillow）重新生成 PNG、ICO 和嵌入的 RGBA 数据，再运行 `build.ps1 -Release` 更新安装版。


[← 返回 README](../README.md)


## UI translations

The Slint UI uses concise English source strings in `@tr()`. Keep values inside placeholders, for example `@tr("Current version {}", root.app-version)`, so translators can reorder them. Chinese wording lives in `ui/zh_CN.po`; keep existing translations unchanged when changing English copy.

Install the matching extractor once (`cargo install slint-tr-extractor --version 1.18.1`), then extract and merge with GNU gettext (or open the POT in a PO editor):

```powershell
slint-tr-extractor --no-default-translation-context -d aeri -o ui/aeri.pot ui/app.slint
msgmerge --update --backup=none ui/zh_CN.po ui/aeri.pot
```

Review new or fuzzy entries, supply their Chinese `msgstr`, and remove fuzzy flags only after review. The POT is an intermediate file; commit `app.slint` and the updated PO. New catalogs should follow the same flat `ui/<locale>.po` convention and be staged by `build.rs` into `<locale>/LC_MESSAGES/aeri.po` in `OUT_DIR`. No MO files or runtime translation files are needed. `build.rs` disables default component contexts and bundles the catalog using `with_bundled_translations`; see the [Slint translation guide](https://docs.slint.dev/latest/docs/slint/guide/development/translations/).

After creating the first Slint component, startup explicitly selects `zh_CN` when `crate::locale::is_zh()` is true, or the English source bundle otherwise. Rust UI copy uses `crate::locale::pick(en, zh)`; `ui_format!` keeps formatted copy checked by Rust. The history viewer receives `crate::locale::code()` during page generation and selects text in JavaScript. `AERI_LANG=en` and `AERI_LANG=zh` override Windows UI language for testing. These language choices never migrate saved configuration values.

Verify with `powershell -ExecutionPolicy Bypass -File build.ps1 -Test` and a debug build using `powershell -ExecutionPolicy Bypass -File build.ps1`. Review chat, each settings category, and expanded advanced options in both languages. Use a temporary APPDATA config with `listen_on_start=false`, `remote_enabled=false`, and `show_window_on_start=true`; stop the installed single-instance app only during testing and restart it afterwards.
