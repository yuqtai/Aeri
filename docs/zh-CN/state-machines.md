# 状态机清单

适用于 Aeri 0.4.0。[English](../state-machines.md)

Aeri 里有不少“这一刻处在哪个阶段”的状态。凡是用几个布尔值、数字或 `Option` 拼出来的，都容易出现**不该存在的组合**：比如“没按着说话键，却在等着开始听手机”。这份清单列出每一个状态机，以及它现在用什么表示、为什么。

**规则**：一个东西有三个以上的阶段，或者有几个布尔值互相牵连，就写成 `enum`，在类型旁边用注释画出转换图。改状态的函数尽量集中在一个地方。

[← 返回中文 README](../../README.zh-CN.md)

## 已经是显式的

| 状态机 | 位置 | 状态 | 说明 |
|---|---|---|---|
| 助手状态 | `assistant::Status` | 待机 / 正在听 / 思考 / 执行 / 播报 / 暂停 | 界面、悬浮球、手机都按它显示。界面那边是数字（0–5），两边注释写明对应关系 |
| **麦克风模式** | `audio::Mode` | Idle / Capture / Muted / Paused / Barge | 原来是 5 个 `u8` 常量，任何数字都能传进去；改成枚举，类型旁边写了谁负责哪一步转换 |
| **手机按住说话** | `audio::PhoneTurn` + `held` | None → Requested → Running → None / Served | 原来是 5 个布尔值（held、active、requested、turn、served），能组合出 32 种情况。现在拆成两个维度：手指按没按着（`held`），和“听手机”这一轮进行到哪一步。`active` 不再单独存，改为由这两者算出来 |
| **悬浮球左键** | `ball::Pointer` | Up / Pressed / Dragging | 原来是“按下位置（Option）+ 是否在拖（bool）”，会出现“没按下却在拖”的组合 |
| **手机控制服务** | `remote::Phase` | Off / Starting / Listening / Failed(原因) | 原来是服务句柄、错误文字、是否在监听三个独立变量。现在带启动编号（generation），被替换掉的旧服务晚到的报错不会覆盖新服务的状态 |
| 后台任务 | `tasks::State` | Running / Done / Failed / Cancelled | 一直是枚举；开启历史保存时，完成结果写入本地历史；运行句柄与续接上下文仍只在当前会话有效 |
| 历史记录清空 | `app.slint` 的 `history-confirm` | 普通 → 等确认（3 秒）→ 普通 | 两个状态，一个布尔值加定时器就够了 |

## 暂时保持原样的

这几处也能算状态机，但现在的写法问题不大，改了收益小、风险大，先不动：

| 状态机 | 位置 | 为什么不改 |
|---|---|---|
| 软件更新按钮 | `main.rs` 的 `UPDATE_CHECK / INSTALL / BUSY` | 要和 Slint 界面共享，Slint 的 `int` 最简单；只有三个值，都集中在 `wire_update` 一处 |
| 设置页未保存提示 | `app.slint` 的 `settings-dirty`、`leave-pending`、`settings-note` | 三个值描述的是不同的事（有改动、正要离开、上次保存的结果），组合基本都合法 |
| 悬浮球显示 | `ball.rs` 的 `enabled`、`visible`、`shown` | `shown` 是由 `visible` 和“前台是否全屏”算出来、缓存下来的，只在 `apply_visibility` 一处更新 |
| 浏览器控制会话 | `browser.rs` 的 `Option<Session>` | 只有“有 / 没有”两种 |
| 桌面快照 | `desktop.rs` 的 `Option<String>` | 只是缓存上一次的快照 |

## 给 AI 智能体的提示

- 想知道助手现在在干什么，看 `assistant::Status`；想知道麦克风的声音会被送去哪里，看 `audio::mode()`。
- 改 `audio::Mode` 的转换前，先读 `audio.rs` 里 `Mode` 上方的转换说明。手机相关的改动要同时跑 `phone_press_starts_at_most_one_turn` 和 `phone_release_before_turn_still_runs_it` 这两个测试。
- 新加一个有阶段的功能时，先写 `enum` 和它的转换图注释，再写逻辑。

标准/本地识别与可选实时语音共用这些音频状态；Capture 不代表一定上传云端。使用 `powershell -ExecutionPolicy Bypass -File build.ps1 -Test` 运行测试，见 [开发说明](development.md)。
