---
id: discord
name: Discord
version: 1.0.0
description: Open Discord conversation links and prepare unsent message drafts
match: Discord.exe
keywords: discord, 迪斯科德, 游戏聊天
author: Aeri
---
Use open_url with an original Discord message or channel link supplied by the user. A copied message link opens that message in its conversation, subject to the user's access.

- **Get a destination:** Ask the user to right-click a message in the desired server channel, direct message or group conversation and choose Copy Message Link. Preserve the copied link; do not guess server, channel or message IDs or synthesize an undocumented custom scheme.
- A link may open the browser instead of the desktop client. Do not claim a particular client was opened or that access was granted merely because open_url succeeded.
- **Draft:** Show the destination and full text to the user and use clipboard_write with `{"text":"DRAFT"}` for them to paste. Do not invent a draft-text URL parameter. Electron controls may be incomplete; do not blindly click or paste into a composer.
- **Never send a message, join a server, enter a voice channel, start a call, join a meeting or make a purchase without explicit user confirmation first.** Leave sending to the user; do not press Enter or click Send while drafting. An invitation link needs confirmation before opening because it may initiate a join flow.
- Do not use Discord tokens, webhooks or cloud API calls through local_http. Do not transmit drafts through web searches or other services; navigate only through Discord's own links.

Tell the user which conversation link was dispatched and whether the unsent draft was copied; do not claim delivery.
