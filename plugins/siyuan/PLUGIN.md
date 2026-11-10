---
id: siyuan
name: SiYuan
version: 1.1.0
description: Write notes, append daily notes, create documents and search using SiYuan's local API
match: SiYuan.exe
keywords: 思源, 思源笔记, siyuan
author: Aeri
---
SiYuan uses Electron and its controls cannot be read. **Do not switch windows, take screenshots or click the interface.** Use local_http for all operations. SiYuan must be running; if closed, launch_app with "SiYuan" (or its installed Start menu name), then wait five seconds before calling the API.

For every request:
- method is POST; url is `http://127.0.0.1:6806/api/...`.
- headers: `{"Authorization": "Token {{secret:~/.config/siyuan/workspace.json#/0|/conf/conf.json#/api/token}}"}`. Copy this placeholder exactly; Aeri resolves the token locally without exposing it to you.
- body is JSON; response `code` 0 indicates success, with content in `data`.
- Use actual newline characters in Markdown content, not the two literal characters backslash and n. Do not add an extra trailing newline.

Common workflows:
1. **Append to today's daily note** (default when the user only asks to save something in SiYuan):
   - `/api/notebook/lsNotebooks`: use the `id` of the first notebook in `data.notebooks` with `closed=false`, or the named notebook if the user specified one.
   - `/api/filetree/createDailyNote`, body `{"notebook": "NOTEBOOK_ID"}`: returns today's daily note `data.id`, including when it already exists.
   - `/api/block/appendBlock`, body `{"dataType": "markdown", "data": "CONTENT", "parentID": "DAILY_NOTE_ID"}`.
2. **Create a document:** `/api/filetree/createDocWithMd`, body `{"notebook": "NOTEBOOK_ID", "path": "/TITLE", "markdown": "CONTENT"}`.
3. **Search notes:** `/api/search/fullTextSearchBlock`, body `{"query": "KEYWORDS"}`. Results are in `data.blocks`, each with `content` and the document's `hPath`.

Write the complete requested content. If the user says to save what was just discussed, organize the previous answer's key points as Markdown instead of merely writing "Saved". Tell the user which notebook and location received the content.
