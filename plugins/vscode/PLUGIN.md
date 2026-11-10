---
id: vscode
name: Visual Studio Code
version: 1.0.0
description: Open local files, project folders and settings in Visual Studio Code
match: Code.exe
keywords: vscode, vs code, visual studio code, 微软代码编辑器
author: Aeri
---
Use open_url with `vscode://` links. Resolve the user's intended absolute path first; never invent a file or workspace.

- **Open a folder:** `vscode://file/c:/Projects/MyProject/` (keep the trailing slash).
- **Open a file:** `vscode://file/c:/Projects/MyProject/src/main.rs`. Add `:LINE:COLUMN` for a known position, for example `vscode://file/c:/Projects/MyProject/src/main.rs:5:10`.
- Use forward slashes in Windows paths. Percent-encode spaces, # and non-ASCII characters in path segments while preserving path separators and the drive colon.
- **Open a setting:** `vscode://settings/SETTING_ID`, for example `vscode://settings/editor.wordWrap`. This navigates to the setting; it does not change its value.
- **Open general settings:** desktop_focus with `{"name":"Visual Studio Code"}`, then desktop_keys with `{"keys":"ctrl+,"}`. If changing a setting is requested, take desktop_snapshot and act only on an identified control with a current reference; settings changes apply immediately.
- Do not accept Workspace Trust, install extensions, run tasks or terminals, or overwrite files merely to open a project. Never add upload or external-service steps.

open_url confirms dispatch only. If the editor does not expose enough information to verify the destination, tell the user what was opened and ask them to check.
