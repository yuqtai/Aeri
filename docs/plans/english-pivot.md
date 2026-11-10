# 调研：Aeri 转向英文为主、面向海外用户

日期：2026-11-10。状态：**调研，尚未开始改**。

## 一、结论先说

能做，而且 Aeri 的核心（Rust、轻量、本机工具、插件机制）不用动。要换的是三层：

1. **语音和大模型**：现在默认全是国内服务（百炼实时语音、百炼识别、DeepSeek），海外用户基本不会有这些账号。要改成服务商可选，默认用海外用户手里已经有的服务。
2. **文字**：界面、提示词、本地直达规则、插件、文档，都要以英文为主、中文为辅。
3. **发布**：代码签名、开源许可证、英文渠道推广。

最大的风险不在技术，而在**第一次运行的门槛**：海外用户下载后，要先注册、填一个实时语音的 Key 才能说话，很多人会在这一步离开。所以第一阶段的重点，是让用户只填一个 Key（OpenAI 或 Google）就能用。

## 二、现状盘点（这次扫代码得到）

| 部分 | 现在 | 海外用户的问题 |
|---|---|---|
| 实时语音 | 百炼 Qwen 实时语音（`dashscope.aliyuncs.com`），音色 `longanqian` | 要注册阿里云；国内节点 |
| 识别 | 百炼 fun-asr | 同上 |
| 大模型 | DeepSeek（Anthropic 兼容接口） | DeepSeek 海外也能用，但不是首选 |
| 唤醒词 | 本地 sherpa-onnx 模型，默认“你好电脑” | **模型本身支持英文**（词表里有英文音素），但代码只做了中文转拼音 |
| 播报 | Windows SAPI | 英文语音系统自带，可以直接用 |
| 本地直达规则 | `fastpath.rs` 约 200 行中文正则 | 要写英文版 |
| 网站搜索 | 抖音、B 站、小红书、淘宝、京东、知乎…… | 要加 Google、YouTube、Amazon、Reddit 等 |
| 界面 | `app.slint` 约 265 行中文 | 要做多语言 |
| 程序内文字 | Rust 里约 1,500 行含中文，其中一大半是给大模型看的工具说明和提示词 | 给人看的要翻译；给大模型看的直接改英文 |
| 插件 | 3 个，全中文 | 要英文版，并补海外常用软件 |
| 文档 | 全中文 | 英文为主 |
| 手机 App | 中文 | 要多语言 |
| 许可证 | **仓库没有 LICENSE 文件** | 法律上等于“保留所有权利”，别人不能合法使用和贡献，也申请不了免费代码签名 |

## 三、每一层怎么改

### 1. 语音和大模型：服务商可选，默认用海外常见的

建议做成三种模式，设置里选：

| 模式 | 听和说 | 想和调工具 | 适合谁 |
|---|---|---|---|
| **OpenAI**（海外默认） | OpenAI Realtime | 同一个 Key 的 GPT 模型 | 大多数人已经有 OpenAI Key，一个 Key 全搞定 |
| **Google** | Gemini Live | Gemini | 便宜；第三方测算的音频单价是几家里最低的 |
| **自定义** | 任意 | 任意 OpenAI 兼容接口（OpenRouter、Groq、本地 Ollama 等）或 Anthropic | 进阶用户 |
| 百炼（中文默认） | 现在这套 | DeepSeek | 国内用户 |

价格参考（第三方整理，各家口径不一致，上线前要以官网为准）：OpenAI 实时语音按 token 计费，折算后每分钟几美分到几十美分不等；Gemini Live 便宜得多；Deepgram、ElevenLabs 这类按分钟算，约 $0.07–0.12/分钟。

**一个值得考虑的卖点：本地离线模式。** 海外用户很在意隐私。sherpa-onnx（现在唤醒词用的就是它）也能跑本地英文识别，播报用 Windows 自带的语音，大模型接本地 Ollama，整套不上网。效果和速度比云端差，但对 Hacker News、Reddit 上的技术用户很有吸引力。建议放在第二阶段。

**代码上**：现在 `realtime.rs` 绑定的是百炼的协议。要抽出一层“实时语音服务商”接口，百炼、OpenAI、Gemini 各实现一份。`llm.rs` 现在只走 Anthropic 兼容接口，要加 OpenAI 兼容接口（几乎所有服务商都支持）。

### 2. 唤醒词：补英文，不用换模型

现在的唤醒词模型（zipformer zh-en）中英文都支持，只需在代码里加“英文单词转音素”这一步。需要 sherpa-onnx 发布包里的英文音素词典 `en.phone`，现在没有放进项目。

默认唤醒词建议用三到四个音节、不常出现在日常对话里的词，比如“Hey Aeri”或“Hey Computer”，需要实测误唤醒率后再定。

### 3. 文字：界面、提示词、直达规则

- **界面**：Slint 自带翻译功能（`@tr()`，可以把翻译直接打包进程序，并能按系统语言自动选择，也能在设置里切换）。英文作为源语言写在 `@tr("...")` 里，中文作为翻译文件。
- **程序里给人看的文字**（报错、提示、播报）：加一个翻译函数统一管理，同样英文为源、中文为翻译。
- **提示词和工具说明**：改成英文一份就够了，大模型读英文效果更好，回答时用用户说的语言。这部分改动最大，但最机械。
- **本地直达规则**：写一套英文的（“open Spotify”“volume to 30”“lock the screen”），按界面语言选用。
- **网站搜索**：按语言提供不同的默认网站列表。

### 4. 插件：英文为主，补海外常用软件

- 插件说明书改成英文为主（大模型读英文更准），名字和简介支持多语言。
- 第一批建议：Notion、Obsidian、Spotify、Microsoft To Do / Outlook、Excel、VS Code、Slack、Discord。其中 Spotify、Notion、Obsidian 都有链接协议或本机/云端接口，适合写成插件。

### 5. 发布和信任

| 事情 | 为什么重要 | 做法 |
|---|---|---|
| **加开源许可证** | 没有许可证，别人不能合法用和贡献；也是申请免费签名的前提 | 选 MIT 或 Apache-2.0（依赖的 agent-desktop 是 Apache-2.0，兼容） |
| **代码签名** | 没签名的 exe，Windows 会弹 SmartScreen“已保护你的电脑”，海外用户很多会直接放弃 | **SignPath Foundation** 给符合条件的开源项目免费签名（要求 OSI 许可证、公开仓库、不含闭源组件、团队开两步验证）。Azure 的签名服务每月 $9.99，但个人只限美国和加拿大，你用不了。注意：签名后 SmartScreen 的信誉仍要慢慢积累，不是马上不弹 |
| **winget** | 海外 Windows 技术用户常用 `winget install` | 给 winget 仓库提交安装清单 |
| **隐私说明** | 海外用户和媒体会问“录音发去了哪里” | 写清楚：唤醒词在本地；只有唤醒后的音频发给你选的服务商；不收集遥测；历史记录只在本机 |
| **名字** | 推广前要确认没撞名 | 简单搜了一下，没发现叫“Aeri”的软件或商标，近似的有 Ari、Aria 等。正式推广前建议查一下美国商标库和域名 |

### 6. 定位和推广

海外同类产品（之前的竞品调研 + 这次补查）：
- **听写类**（Wispr Flow、Superwhisper、OpenWhispr、Handy 等）：只负责把话变成文字，不操作电脑。开源的大多也是听写。
- **Windows 语音访问**：系统自带、免费、离线，但只能说固定命令。
- **Copilot**：能看屏幕，但不替你操作。

这次没找到成熟的、在 Windows 上能真正替你操作软件的开源语音助手。Aeri 可以这样定位：

> **A voice assistant that actually does things on Windows.** 60 MB, open source, bring your own key.

推广渠道，按优先级：
1. **一段 30–60 秒的英文演示视频**，所有渠道都要用。
2. **Hacker News “Show HN”**：技术用户，喜欢开源、轻量、本地。
3. **Reddit**：r/windows、r/software、r/productivity、r/LocalLLaMA（有本地模式之后）。
4. **Product Hunt**：需要准备图片和标语，最好有几位熟人帮忙当天支持。
5. **X / YouTube**：找做 Windows 效率工具的博主。

## 四、分阶段计划

| 阶段 | 内容 | 粗估工作量 |
|---|---|---|
| **0. 打基础** | 加 LICENSE；README 英文为主，中文放 `README.zh-CN.md`；申请 SignPath | 半天，签名审批要等 |
| **1. 能用** | OpenAI 兼容接口的大模型；OpenAI Realtime；英文唤醒词；提示词和工具说明改英文；英文直达规则；按语言选默认服务商和网站列表 | 最大的一块，约 1–2 周 |
| **2. 好用** | 界面多语言（Slint @tr）；程序内文字翻译；首次运行引导（选服务商、填一个 Key、试说一句）；英文插件 + 第一批海外软件插件 | 约 1 周 |
| **3. 推广** | 演示视频；隐私说明；winget；Show HN、Reddit、Product Hunt | 视频制作另算 |
| **4. 加分项** | Gemini Live；本地离线模式；手机 App 多语言 | 按反馈决定 |

## 五、需要你来定的事

1. **许可证**：MIT 还是 Apache-2.0？（我建议 MIT，最简单；如果在意专利条款就选 Apache-2.0）
2. **海外默认服务商**：OpenAI（最多人有 Key）还是 Google（便宜）？
3. **中文用户怎么办**：继续用同一个程序，按系统语言自动切换（我建议这样），还是分成两个版本？
4. **英文唤醒词**：用 “Hey Aeri”，还是更通用的词？
5. **要不要做本地离线模式**：做的话，推广时可以主打隐私。

## 参考资料

- Slint 翻译：[docs.slint.dev — Translations](https://docs.slint.dev/latest/docs/slint/guide/development/translations/)、[select_bundled_translation](https://docs.slint.dev/latest/docs/rust/slint/fn.select_bundled_translation)
- 实时语音价格（第三方整理，以官网为准）：[Cekura — Gemini Live pricing](https://www.cekura.ai/blogs/gemini-live-api-pricing)、[Inworld — voice agent cost per minute](https://inworld.ai/resources/voice-agent-cost-per-minute-2026)、[techstackups — best voice agent 2026](https://techstackups.com/comparisons/best-voice-agent-2026/)
- 代码签名：[Microsoft — code signing options](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options)、[SignPath Foundation](https://signpath.org/)、[SignPath for open source](https://about.signpath.io/product/open-source)
- 唤醒词英文词典：[sherpa-onnx KWS 模型发布页](https://github.com/k2-fsa/sherpa-onnx/releases/tag/kws-models)
- 竞品：[AlternativeTo — Wispr Flow alternatives](https://alternativeto.net/software/wispr-flow/)、[OpenAlternative — Wispr Flow](https://openalternative.co/alternatives/wisprflow)
