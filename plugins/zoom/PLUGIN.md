---
id: zoom
name: Zoom
version: 1.0.0
description: Open confirmed Zoom meeting invitations and prepare unsent chat drafts
match: Zoom.exe
keywords: zoom, zoom workplace, 视频会议, 缩放会议
author: Aeri
---
Use the original Zoom meeting invitation link supplied by the user. The link can include a meeting ID and embedded passcode; preserve it exactly and do not expose its passcode unnecessarily.

- **Meeting:** Confirm the intended meeting and obtain explicit user confirmation to join **before** calling open_url with the invitation link. Opening it may launch Zoom and enter the meeting. Never use a meeting link just to test whether Zoom is installed.
- The browser may ask permission to open Zoom. Only proceed for the confirmed meeting; do not change persistent browser permissions or assume that a waiting room means admission. Do not enable the camera, microphone or screen sharing automatically.
- Do not construct legacy zoommtg links or guess join parameters. If only a meeting ID is known, ask for the original invitation link instead.
- **Chat draft:** Show the intended person/channel and text, then use clipboard_write with `{"text":"DRAFT"}` for the user to paste into Zoom Chat. Do not invent chat deep links or automate an unverified composer.
- **Never send messages, start calls, join meetings or make purchases without explicit user confirmation first.** Do not press Enter or click Send while drafting; leave sending to the user.
- Do not call Zoom's cloud API through local_http or send invitation details or drafts to web searches or other services. Open only Zoom's own invitation links.

Report that the invitation link was dispatched, not that the user joined successfully. A copied chat draft remains unsent.
