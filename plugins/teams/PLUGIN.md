---
id: teams
name: Microsoft Teams
version: 1.0.0
description: Open Teams chats, channels and confirmed meetings and prepare unsent drafts
match: ms-teams.exe, Teams.exe
keywords: microsoft teams, teams, 微软团队, 团队会议
author: Aeri
---
Use open_url with Teams' own links. Use IDs or addresses supplied by the user or an existing copied link; URL-encode query values and path IDs. Do not guess identities or tenant IDs.

- **New chat draft:** `https://teams.microsoft.com/l/chat/0/0?users=EMAIL1,EMAIL2&message=DRAFT`. Encode each email separately, preserving the comma separator. message prefills the compose box; a new chat stays a draft until the first message is sent. Omit message to navigate without prefilled text.
- **Existing chat:** `https://teams.microsoft.com/l/chat/CHAT_ID/conversations`. Get the chat ID from an existing copied message link, not from its display name.
- **Channel:** Open the original channel link copied from Teams. Preserve all IDs and query parameters; do not turn a channel name into an invented URL.
- **Meeting:** Obtain explicit user confirmation to join before opening the original invitation link; opening can initiate a join flow. Do not enable the camera, microphone or screen sharing automatically.
- **Never send messages, start calls, join meetings or make purchases without explicit user confirmation first.** Do not press Enter or click Send while drafting; leave sending to the user. If the draft is not readable, show its full text and use clipboard_write with `{"text":"DRAFT"}` for review.
- Client and browser handling varies. Do not claim access, draft persistence or delivery from link dispatch. Do not call Microsoft Graph through local_http or send content to web searches or other services.

Report the Teams destination opened and whether the text was supplied as an unsent draft.
