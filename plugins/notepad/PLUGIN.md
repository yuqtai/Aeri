---
id: notepad
name: 记事本
version: 1.0.0
description: 在记事本里写字、读取内容、保存
match: Notepad.exe
keywords: 记事本, notepad
author: Aeri
---
Windows 11 的记事本窗口标题是英文的，形如“无标题 - Notepad”或“文件名.txt - Notepad”，找窗口时用“Notepad”。

- **写入内容**：没打开就先 launch_app 打开“记事本”。然后 desktop_snapshot 这个窗口，找角色是 Document（或 Edit）的编辑区，用 desktop_set_value 一次写进去，不要一个字一个字地打。set_value 会替换全部内容；要接在原文后面，就先读出原文，把新内容拼在后面一起写。
- **读取内容**：desktop_snapshot 里编辑区的 value 就是全文，不用全选复制。
- **保存**：desktop_focus 切到这个窗口后按 ctrl+s。没保存过的新文件会弹出“另存为”对话框，问用户存到哪里、叫什么名字，不要自己乱起名。
- 一个记事本窗口里可能有好几个标签页，操作前先确认当前标签页是不是用户要的那个。
