---
id: slack
name: Slack
version: 1.0.0
description: Open Slack workspaces, channels and direct messages and prepare unsent drafts
match: slack.exe
keywords: slack, 思莱克, 工作聊天
author: Aeri
---
Use open_url with Slack's native links. Use real IDs supplied by the user or an existing link; these links do not accept workspace subdomains, channel names or display names as IDs. URL-encode query values.

- **Workspace:** `slack://open?team=TEAM_ID`.
- **Channel:** `slack://channel?team=TEAM_ID&id=CHANNEL_ID`.
- **Direct message:** `slack://user?team=TEAM_ID&id=USER_ID`.
- If only a channel name is known, use `https://slack.com/app_redirect?channel=CHANNEL_NAME&team=TEAM_ID`. Access still depends on membership; do not join a channel or request access automatically.
- **Draft:** Show the proposed recipient and text to the user, then use clipboard_write with `{"text":"DRAFT"}` for them to paste. Do not invent a message-prefill parameter. Electron controls may be incomplete; do not guess composer controls or paste into an unverified conversation.
- **Never send a message, start a call, join a meeting or make a purchase without explicit user confirmation first.** Leave sending to the user; do not press Enter or click Send while drafting.
- Do not use local_http for Slack's cloud API or send content to web searches, webhooks or other services. Navigate only through Slack's own links.

open_url confirms dispatch, not conversation access or delivery. Report the destination opened and whether the draft was copied.
