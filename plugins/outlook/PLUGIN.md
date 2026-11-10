---
id: outlook
name: Outlook
version: 1.0.0
description: Compose an unsent email draft using the Windows mail link handler
match: OUTLOOK.EXE, olk.exe
keywords: outlook, email, 邮件, 邮箱
author: Aeri
---
Use open_url with `mailto:RECIPIENT?subject=SUBJECT&body=BODY` to open an unsent draft. Confirm missing or ambiguous recipients with the user; do not invent addresses.

- URL-encode subject and body separately using UTF-8. Encode spaces as %20, literal & as %26, # as %23 and body line breaks as %0D%0A. Use plain text, not HTML; do not add attachment parameters.
- mailto opens the Windows default mail handler. It may be new Outlook (`olk.exe`), classic Outlook (`OUTLOOK.EXE`) or another app; the link cannot select a particular Outlook edition or sending account. Ask the user to set their preferred MAILTO handler if the wrong app opens; do not change defaults automatically.
- Check the recipient, subject, body and From account in the draft with desktop_snapshot if exposed. Use current control references; if the fields cannot be read, ask the user to review them. Never claim that a draft was saved or sent merely because open_url succeeded.
- **Never send without explicit user confirmation of the final recipients and content.** Do not press Enter, ctrl+enter, alt+s or click Send while drafting. If safe sending controls cannot be verified, leave sending to the user even after confirmation.
- Do not call mail services through local_http, upload attachments or transmit message content to other internet services. Use only the mail link for the requested draft.

Tell the user the draft link was opened for review, not that the email was sent.
