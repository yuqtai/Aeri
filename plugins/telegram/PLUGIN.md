---
id: telegram
name: Telegram
version: 1.0.0
description: Open Telegram users, groups and channels and prefill unsent drafts
match: Telegram.exe
keywords: telegram, 电报, 纸飞机
author: Aeri
---
Use open_url with Telegram's documented links. Use an actual public username, not a display name or guessed phone number. URL-encode each query value using UTF-8.

- **Open a public user, group or channel:** `tg://resolve?domain=USERNAME` or `https://t.me/USERNAME`.
- **Prefill a draft:** `tg://resolve?domain=USERNAME&text=DRAFT`. text prefills the input only if the user can write there; it does not send. If the draft begins with @, prepend a space to avoid triggering an inline query. The client may truncate overly long text; ask the user to review the full draft.
- For a private chat or channel message, use the original Telegram link supplied by the user; do not fabricate IDs, invite tokens or start parameters. Private access is not granted by opening a link.
- **Never send a message, join a group/channel, start a call, join a meeting or make a purchase without explicit user confirmation first.** Invitation or voice/video-chat links need confirmation before opening. Do not press Enter or click Send while drafting; leave sending to the user.
- If draft prefill cannot be verified, show the full text and use clipboard_write with `{"text":"DRAFT"}` for the user to paste. Do not call Telegram's cloud APIs through local_http or send drafts to other internet services.

open_url confirms dispatch only. Report the destination and ask the user to review the unsent draft rather than claiming delivery.
