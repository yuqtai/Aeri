# 使用说明

适用于 Aeri 0.4.0。[English](../guide.md)

## 第一次使用

1. 在 [Releases](https://github.com/yuqtai/Aeri/releases/latest) 下载 Windows x64 ZIP，解压后运行 `aeri.exe`。
2. 打开 **设置 → 大模型**，选择服务商、填写 Key，保存并应用。中文新安装默认 DeepSeek，英文新安装默认 OpenAI；Ollama 可连接本地模型，不需要 Key。
3. **语音对话 → 对话方式** 保持 **标准模式（推荐）**，识别服务保持 **本地（离线）**。可先点“测试语音识别”下载并加载模型。
4. 中文新安装说 **“你好电脑”**，英文新安装说 **“Hey Aeri”**。也可以点光球、按 Ctrl+Alt+空格，或者打字。

本地识别首次使用时从上游下载对应语言的 sherpa-onnx 模型，核对 SHA-256 后存到 `%APPDATA%\VoiceAssistant\models`。首次下载需要网络和 Windows `tar.exe`，以后识别可离线运行。标准模式用 Windows SAPI 播报；大模型仍需要所选服务的网络连接，除非使用本地服务。

语言跟随 Windows **显示语言**：中文显示语言使用中文，其他语言使用英文。启动进程前设置 `AERI_LANG=en` 或 `AERI_LANG=zh` 可覆盖。语言影响新安装的默认值（包括唤醒词），**不会覆盖已有 `config.json` 保存的服务、模型、唤醒词或其他值**，升级也不会强制改成新默认。

## 界面

- **悬浮球**：单击展开主界面，关闭主界面缩回光球；拖动可移动，靠近左右边缘会吸附并记住位置。前台全屏时自动隐藏，后台任务运行时显示绿点。
- **右键菜单和托盘**：打开窗口、开始说话/停止、暂停或恢复唤醒、隐藏或显示光球、退出。关闭主窗口不等于退出。
- **对话页**：主光球显示待机、聆听、思考、执行、播报、暂停。忙时再点一下可停止当前轮。用户消息、回答、工具活动和后台进度分别显示；左下角控制语音唤醒，右下角展开打字框。
- **设置页**：共有九类，少用的参数放在“高级选项”。点“保存并应用”生效；没保存离开时可以保存或放弃。“恢复默认设置”保留已存 Key。

| 分类 | 设置内容 |
|---|---|
| 语音对话 | 标准/实时模式、识别/实时服务与测试、语速、继续聆听、唤醒词打断、耳机插话、降低其他音量；高级选项有模型、地址、音色、断句静音和等待开口时长 |
| 唤醒与麦克风 | 麦克风选择/刷新、唤醒词、灵敏度、启动唤醒、Ctrl+Alt+空格；高级选项有唤醒词权重 |
| 大模型 | 服务商、Key/测试/清除、模型、思考和强度；高级选项有接口地址、新对话间隔 |
| 电脑操控 | 桌面/浏览器开关，Codex/Claude Code/内置执行者，完全访问/工作目录/只读；高级选项有目录、智能体模型、浏览器闲置时间和 Playwright 包 |
| 手机控制 | 局域网控制开关、配对、撤销全部配对；端口在配置文件 `remote_port`，默认 47821 |
| 外观与启动 | 跟随系统/浅色/深色、悬浮球、开机启动、启动时打开主界面 |
| 软件更新 | 检查与安装、每天自动检查 |
| 历史记录 | 查看/搜索、是否保存、清空聊天和任务记录 |
| 插件 | 浏览、安装、更新、卸载应用说明书 |

配置在 `%APPDATA%\VoiceAssistant\config.json`，通常不用手改。唤醒词应有辨识度；英文词使用发音词典，中文转拼音 token。不支持的英文发音会给出警告。

## 服务与 Key

下表是 **0.4.0 源码预设**，不保证服务商以后仍提供这些模型。可根据账号权限改模型或高级地址。

| 大模型服务 | 预设模型 | 环境变量后备 |
|---|---|---|
| OpenAI | `gpt-4.1-mini` | `OPENAI_API_KEY` |
| Anthropic | `claude-sonnet-5-5` | `ANTHROPIC_API_KEY` |
| Google Gemini | `gemini-3.8-flash` | `GEMINI_API_KEY` |
| OpenRouter | `openai/gpt-4.1-mini` | `OPENROUTER_API_KEY` |
| Groq | `openai/gpt-oss-120b` | `GROQ_API_KEY` |
| DeepSeek | `deepseek-flash` | `DEEPSEEK_API_KEY` |
| Ollama | `qwen3`，`http://localhost:11434/v1` | 不需要 Key |
| 自定义 | `default`，`http://localhost:8000/v1` | 自行配置，Key 可选 |

兼容 OpenAI 的服务使用 Chat Completions；Anthropic 和 DeepSeek 预设使用 Anthropic 消息格式。

| 语音路径 | 0.4.0 支持 |
|---|---|
| 标准模式识别 | 本地 sherpa-onnx；OpenAI `gpt-transcribe`；Groq `whisper-large-v3-turbo`；百炼 `fun-asr-realtime` |
| 标准模式播报 | Windows SAPI，选择可见的中/英文匹配音色；同语言优先 Natural/Neural（必须能被 SAPI 枚举） |
| 可选实时语音 | OpenAI `gpt-realtime-2` / `marin`；百炼 `qwen-audio-3.1-realtime-plus` / `longanqian_v3.1` |

实时模式将音频发给所选服务转写、播报；配置了大模型 Key 时由大模型思考和调用工具，否则可由实时模型处理。打字仍走标准路径，实时连接失败可回退标准识别。

OpenAI 识别和实时可共用已保存的 OpenAI 大模型 Key 或 `OPENAI_API_KEY`，单独填的语音 Key 优先。Groq 识别可用 `GROQ_API_KEY`；百炼用 `DASHSCOPE_API_KEY`。保存的 Key 优先于环境变量，使用当前 Windows 账户的 DPAPI 加密。

Codex、Claude Code 使用各自 CLI 的安装和登录，需要单独完成；内置执行者使用 Aeri 当前所选大模型。

## 历史和后台任务

默认保存聊天与完成的任务记录 30 天，位置为 `%APPDATA%\VoiceAssistant\history\YYYY-MM-DD.jsonl`。启动时恢复最近聊天用于显示。“查看历史记录”生成可按天浏览和搜索的本地网页。当前清理逻辑在启动时执行，以最新保存日期为基准保留 30 天，因此长期停用时旧记录可能保留到后续清理。关闭保存只停止新记录；要删除旧记录，点“清空”再点“确认清空”（三秒后确认自动取消）。

一次只运行一个后台任务，可以继续聊天。当前运行期间可要求继续或重试旧任务并带上上下文。重启后仍可搜索保存的结果，但不会恢复运行中的任务或内存中的续接状态。提醒单独保存在 `reminders.json`，重启后仍有效。

## 手机控制（安卓 App）

手机和电脑在同一局域网直接连接，不经过中转服务器。支持按住说话、文字、停止、暂停/恢复唤醒、音量、静音和媒体控制；回答从电脑播出，电脑没有麦克风也可用手机说话。

1. 电脑开启 **设置 → 手机控制** 并保存；防火墙询问时只允许专用网络。
2. 手机安装 Release 的 `Aeri-vX.Y.Z-android.apk`，或电脑程序旁的 `android\Aeri.apk`，使用 Android 8 及以上并连接同一 Wi-Fi。
3. 电脑点“配对新手机”，手机输入六位配对码。配对后记住电脑，并支持局域网发现。

撤销全部配对会断开手机并要求重新配对。App 切到后台会断开。电脑带有新版 APK 时，手机可从电脑接收、校验后打开系统安装界面；更新时必须保持签名证书一致。协议见 [android/PROTOCOL.md](../../android/PROTOCOL.md)。

连接**不加密**，仅在可信任的私人局域网开启，详见 [安全与已知限制](safety.md)。

[← 返回中文 README](../../README.zh-CN.md)
