# Safety and known limitations

Applies to Aeri 0.4.9. [简体中文](zh-CN/safety.md)

## Access and data

- Desktop and browser control can be disabled separately in settings.
- The background executor defaults to **Full access**. It can change files and run commands without per-action approval. A misheard request can have consequences. Choose **Working directory** or **Read only** in **Settings → Computer control** to restrict access — these modes are passed to Codex (`workspace-write` / `read-only` sandbox) and Claude Code permissions, and are **also hard-enforced at Aeri's own tool layer** (shared by MCP and the built-in assistant):
  - **Read only**: blocks all side-effect tools (file rename/move/copy/delete, closing apps, desktop click/type/shortcut, browser actions, volume/system toggles, power actions), allowing only inspection, screen-reading, and query tools.
  - **Working directory**: validates that every file-modifying tool target resolves inside the configured working directory (rejecting out-of-bounds paths), enforcing consistent boundaries across Codex, Claude Code, and the built-in assistant.
- Within Full access or allowed write paths, background instructions still tell agents to stop before irreversible actions such as deleting files, sending messages, paying, or submitting forms. That part is a prompt constraint, not a security boundary. A task's returned text is not proof that every requested action succeeded.
- Task requests go to external CLIs over standard input, not through shell interpolation. Cancelling a task or quitting Aeri terminates the external agent process tree.
- API keys are stored in `%APPDATA%\VoiceAssistant\config.json` as plain text so Settings can reveal them behind the eye button (older DPAPI-encrypted keys are converted on load); use OS environment variables instead if you prefer not to store keys in `config.json`. Chat and task history is local, unencrypted, and retained for 30 days when enabled. It may contain sensitive request and result text; view, disable, or clear it in **History** settings.

Wake-word detection stays local. In Standard mode with local recognition, microphone audio is recognized locally after a one-time model download; recognized text, screen context, and tool results may still go to the configured language model. Cloud recognition and realtime send audio to the selected provider after activation, including follow-up turns. Headphone interruption keeps the microphone active during speech; leave it off with speakers to avoid feedback. Provider billing and data policies apply to those requests.

Plugins are instructions, not executable extensions, but can influence tool use. Install only trusted guides. `local_http` restricts HTTP(S) hosts to loopback; secret placeholders are resolved locally and their values redacted from returned text. This restriction belongs to that tool, not to every tool an agent may use. See [plugins](plugins.md).

## Phone access

Phone control is off by default and accepts only local/LAN addresses when enabled. Pairing uses a six-digit code valid for five minutes; five wrong guesses invalidate it. Paired phones use a random 256-bit token protected on the PC with DPAPI in `remote.json`. The connection itself is **unencrypted**, so audio, commands, and replies must travel only over a trusted private LAN. Revoke all pairings to remove access.

## Desktop and browser limits

- Desktop controls normally use Windows accessibility APIs without moving the pointer. Real keyboard typing, shortcuts, and double-clicks still share your keyboard/mouse; avoid competing input during those operations.
- A normal app cannot control elevated windows. Minimized windows may require restoring before a controls snapshot can read them. Some custom UI and popup dropdowns are not exposed reliably.
- The desktop helper rejects certain system shortcuts; dedicated tools handle supported app closing and system actions. Closing an app first requests a normal close; save prompts are left for you. Protected system processes are not closed.
- The helper requires a user-only writable `%USERPROFILE%\.agent-desktop` directory. If a CLI sandbox adds another writable account, Aeri attempts to repair those extra permissions and retry.
- Browser automation uses a separate Edge session, without your normal browser logins. It needs Node.js/npx and Playwright MCP. Site-search shortcuts instead open results in your own browser.
- File search prefers the Windows Search index. Unindexed locations fall back to scanning common folders, not every folder on every drive.
- Clipboard history contains text copied while Aeri is running and stays in memory. Conversation/task history is a separate, persisted feature.
- Windows does not expose direct toggles for every setting: Night light and Do not disturb open Settings; brightness control generally needs a built-in display.

## Voice and operational limits

Local recognition needs internet for the first model download, sufficient disk space, and Windows `tar.exe`. Its language follows the app language, not automatic multilingual detection. Standard speech uses SAPI-visible voices: installing a Windows Natural voice does not guarantee SAPI exposes it. Realtime and cloud recognition need their provider's network service and key. You can type when voice is unavailable; model-based chat still needs a working LLM.

Wake detection can miss speech or trigger incorrectly in noise. Adjust sensitivity and use the hotkey when needed. Microphone changes are handled automatically; if none is available the app retries periodically. Realtime spoken requests bypass Standard fast-path rules; typed requests still take the Standard path. The realtime service may begin its own response, which Aeri cancels when using the configured LLM's answer.

Only one background task runs at a time. In-memory task continuation is session-local; persisted history provides old results, not restored running jobs. Failures, empty responses, and step-limit exhaustion are reported, but review important outcomes yourself.

## Checks still needing real hardware or interaction

The floating orb supports multi-monitor per-monitor DPI scaling, same-monitor full-screen detection, and click-through on its outer halo, though unusual monitor layouts may still benefit from dragging it into place. Earlier project notes did not establish physical reboot autostart verification, or real hardware toggles for Wi-Fi, Bluetooth, brightness, theme, and power across varied laptops. They also recorded an intermittent first-click issue after simulated typing and had not established whether physical typing/pasting reproduced it. Wake-word/talk interruption with loud speakers still benefits from testing in your own room acoustics.

[User guide](guide.md) · [← README](../README.md)
