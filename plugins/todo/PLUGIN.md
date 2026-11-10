---
id: todo
name: Microsoft To Do
version: 1.0.0
description: Find and carefully add tasks in Microsoft To Do using visible controls
match: Todo.exe
keywords: microsoft to do, microsoft todo, 微软待办, 待办事项
author: Aeri
---
Use launch_app with `{"name":"Microsoft To Do"}` if closed. Use desktop_snapshot to identify the intended account and list before changing tasks; ask if the destination is ambiguous.

- **Read or search:** Read exposed task text from desktop_snapshot. For search, desktop_focus on the verified To Do window, then desktop_keys with `{"keys":"ctrl+f"}`. Take a fresh snapshot and set only the identified search input with desktop_set_value (`ref` and `value`). Do not claim to have read hidden or inaccessible tasks.
- **Add a task:** Confirm the requested list, select it using desktop_click with a current ref, and take a fresh snapshot. Find the Add a task input (its label may be localized). Use desktop_set_value with its `ref` and the task title as `value`; this replaces existing input, so do not overwrite an unrelated draft.
- Recheck the list and input text, focus the verified window and click that input to focus it, then desktop_keys with `{"keys":"enter"}` to create the task. Take a fresh snapshot to verify the title appeared in the list. If the outcome is unclear, stop and ask the user to check rather than adding it twice.
- Due dates, reminders and notes need the task's detail view. Only change an explicitly requested field that can be identified in a fresh snapshot; do not guess controls or claim a reminder was scheduled from title text alone.
- If the app does not expose usable controls, use clipboard_write with `{"text":"TASK_TITLE"}` and ask the user to paste into Add a task and press Enter. There is no task-creation deep link used by this guide.
- Do not complete, delete or share tasks merely to add one. Messages, meeting joins and purchases require explicit user confirmation first. Do not call Microsoft Graph through local_http or transmit task text to web searches or other services.

Report the list and verified task title, or clearly state that only text was copied and task creation needs the user.
