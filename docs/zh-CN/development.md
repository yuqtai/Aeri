# 开发、编译与发版

适用于源码版本 0.4.0。[English](../development.md)

## 架构

```text
麦克风 → 16 kHz 降采样 → 能量门限 → 本地唤醒模型
  → 标准（新安装默认）：本地/云识别 → 直达规则或所选大模型 → Windows SAPI
  → 可选实时：OpenAI/百炼音频 → 所选大模型与工具 → 实时语音播报
  → 长任务：Codex / Claude Code / 内置执行者 → MCP 工具 → 返回结果
```

唤醒检测使用本地 sherpa-onnx KWS。标准模式默认本地 ASR，首次另行下载并校验模型。实时模式向所选服务传音频，OpenAI 输入重采样到 24 kHz，输出支持中断。播报时可只听唤醒词；戴耳机可打开全程收音插话。

对话时其他应用音量默认降至 20%，结束后恢复；用户期间手调过的不覆盖。崩溃恢复数据用于下次启动恢复音量。

| 文件 | 作用 |
|---|---|
| `src/main.rs`、`ui/app.slint` | 界面、托盘、设置；`--mcp`、`--kws-test` 入口 |
| `src/locale.rs`、`ui/zh_CN.po`、`build.rs` | Windows 显示语言、Rust 文案、内嵌 Slint 翻译 |
| `src/config.rs` | 新安装与旧配置默认值分离、保存、DPAPI |
| `src/assistant.rs`、`src/audio.rs`、`src/kws.rs`、`src/resample.rs` | 调度、音频路由、唤醒、重采样 |
| `src/asr.rs`、`src/tts.rs` | 本地/OpenAI/Groq/百炼识别；Windows SAPI |
| `src/realtime.rs`、`src/player.rs` | OpenAI/百炼 WebSocket 与可中断的 24 kHz 播放 |
| `src/llm.rs`、`src/agent.rs`、`src/fastpath.rs` | 多服务预设、工具循环、本地规则 |
| `src/tasks.rs`、`src/mcp.rs` | 后台执行者、标准输入输出 JSON-RPC 工具 |
| `src/tools/`、`src/plugins.rs`、`plugins/` | 本地工具、桌面/浏览器适配、应用指南和市场 |
| `src/history.rs`、`src/reminders.rs` | 30 天本地记录、持久提醒 |
| `src/ball.rs`、`src/sys.rs`、`src/screen.rs` | Win32 光球、系统集成、前台上下文 |
| `src/remote.rs`、`android/` | 局域网手机服务、Kotlin/Compose 客户端；见 [协议](../../android/PROTOCOL.md) |
| `src/update.rs` | 下载校验、替换/回滚、重启 |

修改音频或生命周期前先看 [状态机](state-machines.md)。

## Windows 准备

安装 rustup/Rust、VS 2022 C++ 生成工具和 Windows SDK。**通过 build.ps1 编译和测试**，它负责加载 MSVC 环境、补充 SDK 路径；未配置环境直接运行 Cargo 可能链接失败。

唤醒模型不在 Git 中。到上游 [KWS 模型发布页](https://github.com/k2-fsa/sherpa-onnx/releases/tag/kws-models) 下载 `sherpa-onnx-kws-zipformer-zh-en-3M-2025-12-20.tar.bz2`，内容放到 `models\kws`。运行所需文件：

- `encoder-epoch-13-avg-2-chunk-16-left-64.int8.onnx`
- `decoder-epoch-13-avg-2-chunk-16-left-64.onnx`
- `joiner-epoch-13-avg-2-chunk-16-left-64.int8.onnx`
- `tokens.txt` 和仓库跟踪的英文词典 `en.phone`（解压时保留）

这是随程序分发的唤醒模型，和首次使用下载到用户数据目录的 ASR 模型不是同一个。

## 调试与测试

在仓库根目录运行：

```powershell
powershell -ExecutionPolicy Bypass -File build.ps1 -Test
powershell -ExecutionPolicy Bypass -File build.ps1
```

运行 `python docs/check_links.py` 可检查两个 README 和 `docs` 下全部 Markdown 的相对链接，包括图片和标题锚点。

默认输出为 `%USERPROFILE%\.cargo-target\va\debug`。已有 `CARGO_TARGET_DIR` 时尊重该值；并行开发或需要短路径时先设置：

```powershell
$env:CARGO_TARGET_DIR = 'C:\aeri-target'
powershell -ExecutionPolicy Bypass -File build.ps1
```

首次编译下载固定的 agent-desktop v1.0.2，校验 SHA-256，缓存到 `vendor`，再把组件和运行模型复制到 exe 旁。浏览器另需 Node.js/npx 和 Playwright MCP。运行时保留旁边的 DLL 和模型；`-Run` 可编译后启动，`aeri.exe --kws-test a.wav b.wav` 可检查 WAV 唤醒。

**`build.ps1 -Release` 有安装副作用**：生成 `dist`，可能停止在该目录运行的旧程序；存在安装标记时还会更新并启动已安装版。日常开发和验证使用调试构建。

## Android

需要 JDK 17+、Android SDK，设置 `ANDROID_HOME`/`ANDROID_SDK_ROOT` 或 `android/local.properties` 的 `sdk.dir`。在 `android` 目录运行 `./gradlew.bat assembleRelease`，输出为 `android/app/build/outputs/apk/release/app-release.apk`。

Android 构建请使用 **纯 ASCII 路径**。Android 工具可能拒绝含中文等非 ASCII 字符的项目路径；`CARGO_TARGET_DIR` 只移动 Rust 输出，不能解决 Android 项目路径问题。

`build.ps1 -Android` 可在调试版带上 App；发布版总会尝试构建。没有 JDK/SDK 则警告并跳过。脚本把 `Aeri.apk` 和版本/大小/SHA-256 元数据 `Aeri.json` 放到 exe 旁的 `android`。版本名与编号取自 `Cargo.toml`。

当前 release 类型使用 `%USERPROFILE%\.android\debug.keystore` 签名。换构建电脑时保留同一证书，否则不能覆盖更新，必须卸载重装。不要提交私钥。

## 安装与更新

`install.ps1 -Launch` 把已准备的发布文件安装到 `%LOCALAPPDATA%\Programs\VoiceAssistant`，创建快捷方式与卸载入口，不需要管理员。设置、Key、提醒、插件和历史在 `%APPDATA%\VoiceAssistant`，安装更新与卸载均保留。

开机启动使用当前用户 Run 注册表项。移动程序后启动一次以更新登记路径；自启通常只显示光球。

应用内更新从 GitHub Release 下载，核对附件摘要，解压到 `%TEMP%\aeri-update`，等待旧进程退出后替换，失败回滚，随后重启。每天检查只通知，不静默安装。也可用 `aeri.exe --update`。

## 维护者发版

本节描述发布，不属于调试/测试。提交版本与全部改动、保持工作区干净后，维护者可执行 `scripts/release.ps1 -Notes <说明文件>`。脚本默认调用发布构建，打包 `dist` 与第三方声明，写 SHA-256、创建版本标签，**推送 HEAD 到 main 和标签**，再用 `gh` 创建 Release。

不要在功能分支为测试打包而运行；`-SkipBuild` 只跳过编译，仍会发布。自动更新依赖 `Aeri-vX.Y.Z-windows-x64.zip`，同时上传 `.sha256` 和存在时的 `Aeri-vX.Y.Z-android.apk`，不要改附件名。

## UI 翻译与配置兼容

Slint 的 `@tr()` 使用简洁英文源文案，把变量留在占位符中方便重排。中文在 `ui/zh_CN.po`。用匹配版本的提取器和 GNU gettext：

```powershell
cargo install slint-tr-extractor --version 1.18.1
slint-tr-extractor --no-default-translation-context -d aeri -o ui/aeri.pot ui/app.slint
msgmerge --update --backup=none ui/zh_CN.po ui/aeri.pot
```

审核新条目和 fuzzy 后提交 Slint 与 PO，不提交中间 POT。`build.rs` 目前把 `ui/zh_CN.po` 复制为 `OUT_DIR` 中的 `zh_CN/LC_MESSAGES/aeri.po` 并内嵌，无需运行时 MO 或默认组件上下文；新增语言需扩展该复制逻辑。

启动根据 `locale::is_zh()` 选择中文包或英文源。Rust 使用 `locale::pick(en, zh)`，格式化用 `ui_format!`；历史页接收 `locale::code()`。使用隔离的 APPDATA，分别设置 `AERI_LANG=en`/`zh` 检查；界面测试配置可设 `listen_on_start=false`、`remote_enabled=false`、`show_window_on_start=true`，注意单实例限制。

**绝不能用新安装默认值覆盖用户已有配置。** `Config::new_install` 为两种语言选择标准/本地识别，英文选择 OpenAI 大模型和实时服务，中文保留 DeepSeek/百炼。旧配置反序列化保留原服务和模式；修改前阅读兼容性测试。

## 图标

品牌源为 `assets/app.svg`，Windows 图标为 `assets/app.ico`。`python scripts/generate_icon.py`（需 Pillow）生成 PNG、ICO 和内嵌 RGBA；可调试构建检查。

[← 返回中文 README](../../README.zh-CN.md)
