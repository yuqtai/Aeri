---
id: obsidian
name: Obsidian
version: 1.0.0
description: 新建笔记、追加到每日笔记、打开或搜索笔记，用 Obsidian 自带的链接协议，不用点界面
match: Obsidian.exe
keywords: obsidian, 黑曜石
author: Aeri
---
Obsidian 是 Electron 做的，读不到界面控件，**不要切窗口、截图、点界面**。用 open_url 打开 `obsidian://` 链接，Obsidian 会自己执行（没开会自动打开）。链接里的中文、空格、换行都要做 URL 编码（换行是 %0A）。

- **新建笔记**：`obsidian://new?name=标题&content=内容`。用户说了库名就加 `&vault=库名`，没说就不加（用最近打开的库）。同名笔记已存在时会新建一个带编号的，不会覆盖。
- **追加到已有笔记**：`obsidian://new?file=笔记路径&content=内容&append=true`
- **追加到今天的每日笔记**：`obsidian://daily?content=内容&append=true`（需要用户开着“日记”核心插件）
- **打开笔记**：`obsidian://open?file=笔记路径`
- **搜索**：`obsidian://search?query=关键词`（在 Obsidian 里显示结果，你读不到结果，告诉用户去看）

open_url 只能确认链接发出去了，确认不了 Obsidian 是否真的写成功；告诉用户“已经发给 Obsidian”，并请对方看一眼。
