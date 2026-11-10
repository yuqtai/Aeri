---
id: obsidian
name: Obsidian
version: 1.1.0
description: Create notes, append daily notes, open and search notes using Obsidian links
match: Obsidian.exe
keywords: obsidian, 黑曜石
author: Aeri
---
Obsidian uses Electron and its controls cannot be read. **Do not switch windows, take screenshots or click the interface.** Use open_url with `obsidian://` links; Obsidian handles them and launches automatically if closed. URL-encode Chinese text, spaces and line breaks (a newline is %0A).

- **Create a note:** `obsidian://new?name=TITLE&content=CONTENT`. If the user names a vault, add `&vault=VAULT`; otherwise omit it to use the most recently opened vault. Existing names create a numbered note instead of overwriting.
- **Append to a note:** `obsidian://new?file=NOTE_PATH&content=CONTENT&append=true`
- **Append to today's daily note:** `obsidian://daily?content=CONTENT&append=true` (requires the Daily notes core plugin).
- **Open a note:** `obsidian://open?file=NOTE_PATH`
- **Search:** `obsidian://search?query=KEYWORDS` (results appear in Obsidian and cannot be read here; tell the user to view them).

open_url only confirms the link was sent, not that Obsidian wrote successfully. Tell the user it was sent to Obsidian and ask them to check.
