---
id: notepad
name: Notepad
version: 1.1.0
description: Write, read and save text in Notepad
match: Notepad.exe
keywords: 记事本, notepad
author: Aeri
---
Windows 11 Notepad window titles include "Notepad", for example "Untitled - Notepad" or "filename.txt - Notepad". Use "Notepad" to find the window.

- **Write:** If it is closed, first launch_app with "Notepad". Take desktop_snapshot, find the Document (or Edit) editor and use desktop_set_value to write the entire text at once instead of typing character by character. set_value replaces all content; to append, read the existing text and write the combined text.
- **Read:** The editor's value in desktop_snapshot contains the full text; do not Select All and copy.
- **Save:** Use desktop_focus, then ctrl+s. A new unsaved file opens Save As: ask the user for the location and file name rather than inventing them.
- A window can contain multiple tabs. Before acting, verify that the current tab is the one the user wants.
