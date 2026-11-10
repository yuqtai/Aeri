---
id: notion
name: Notion
version: 1.0.0
description: Open Notion page links and search the current workspace
match: Notion.exe
keywords: notion, notion笔记, 诺申
author: Aeri
---
Use open_url with an original Notion page link supplied by the user. Preserve the page ID and any block fragment; do not invent page IDs or append undocumented search parameters.

- **Open a page:** Dispatch the copied Notion HTTPS link. Notion's link-opening preferences determine whether it opens in the browser or desktop app; on the Windows MSIX app, Apps for websites also affects handling. Do not change these preferences automatically.
- **Search:** In the installed app, use desktop_focus with `{"name":"Notion"}`, then desktop_keys with `{"keys":"ctrl+p"}`. Take desktop_snapshot; if an identifiable search input is exposed, use desktop_set_value with its current ref and `{"value":"QUERY"}`. Read results only when exposed, and verify the page title before opening one.
- Notion's Electron controls may be incomplete. If search input or results cannot be identified, copy the query using clipboard_write with `{"text":"QUERY"}` and ask the user to paste it into Notion search. Do not type or paste blindly into a page body, or claim unreadable results were found.
- Notion's cloud API is not reachable through local_http, which is limited to localhost/127.0.0.1. Do not request integration secrets, invent a local endpoint or upload content to a cloud service.
- This guide covers opening and searching, not publishing or sharing pages. Any purchase, message sending or meeting join needs explicit user confirmation first.

open_url confirms dispatch only. Report the page link opened or search prepared and any result that still needs the user's review.
