---
id: siyuan
name: 思源笔记
version: 1.0.1
description: 记笔记、追加到今天的日记、新建文档、搜索笔记，直接走思源的本机接口，不用点界面
match: SiYuan.exe
keywords: 思源, siyuan
author: Aeri
---
思源是 Electron 做的，读不到界面控件，**不要切窗口、截图、点界面**。全部用 local_http 调思源的本机接口（思源要开着；没开先用 launch_app 打开“思源笔记”，等 5 秒再调）。

每个请求都这样发：
- method 用 POST，url 是 `http://127.0.0.1:6806/api/...`
- headers：`{"Authorization": "Token {{secret:~/.config/siyuan/workspace.json#/0|/conf/conf.json#/api/token}}"}`（令牌由 Aeri 在本机读取后填进去，你看不到它，原样照抄这串即可）
- body 是 JSON；返回里 `code` 为 0 表示成功，内容在 `data` 里
- Markdown 内容里的换行就是普通换行，不要写成反斜杠加 n 这两个字符，结尾也不要多加换行符

常用做法：
1. **记到今天的日记**（用户只说“记到思源里”时用这个）：
   - `/api/notebook/lsNotebooks` 拿笔记本列表，用 `data.notebooks` 里第一个 `closed` 为 false 的 `id`（用户说了笔记本名就用对应的）
   - `/api/filetree/createDailyNote`，body `{"notebook": "笔记本id"}`，得到今天日记的 `data.id`（已存在会直接返回）
   - `/api/block/appendBlock`，body `{"dataType": "markdown", "data": "要记的内容", "parentID": "日记id"}`
2. **新建一篇文档**：`/api/filetree/createDocWithMd`，body `{"notebook": "笔记本id", "path": "/标题", "markdown": "内容"}`
3. **搜索笔记**：`/api/search/fullTextSearchBlock`，body `{"query": "关键词"}`，结果在 `data.blocks`，每条有 `content` 和所在文档 `hPath`

要记的内容写完整：用户说“把刚才的记下来”，就把上一轮回答的要点整理成 Markdown 写进去，不要只写一句“已记录”。写完告诉用户记到了哪个笔记本的哪里。
