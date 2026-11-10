<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/hero-en-dark.png">
    <img src="docs/screenshots/hero-en-light.png" alt="Aeri: just say Hey Aeri, it does the rest">
  </picture>
</p>

<p align="center">
  <a href="https://github.com/yuqtai/Aeri/releases/latest"><img src="https://img.shields.io/github/v/release/yuqtai/Aeri?style=flat-square&color=4F46E5" alt="Release"></a>
  <img src="https://img.shields.io/badge/Windows-10%20%7C%2011-0078D4?style=flat-square" alt="Windows">
  <img src="https://img.shields.io/badge/license-MIT-22C55E?style=flat-square" alt="MIT">
  <img src="https://img.shields.io/badge/Rust-000000?style=flat-square&logo=rust" alt="Rust">
</p>

<p align="center">
  <b>English</b> · <a href="README.zh-CN.md">简体中文</a>
</p>

<p align="center">
  <a href="https://github.com/yuqtai/Aeri/releases/latest"><b>Download</b></a> ·
  <a href="#quick-start"><b>Quick start</b></a> ·
  <a href="docs/plugins.md"><b>Plugins</b></a> ·
  <a href="https://github.com/yuqtai/aeri-plugins"><b>Plugin market</b></a>
</p>

<br>

<h3 align="center">Just say "Hey Aeri." It does the rest.</h3>

<table>
<tr>
<td width="33%" valign="top">
<h4>🎙️ Hands-free</h4>
Say <b>"Hey Aeri"</b> and what you need. It opens apps, renames files, changes settings and sets reminders. No clicks, no keyboard. It also speaks up on its own when a reminder is due or a background job is done.
</td>
<td width="33%" valign="top">
<h4>🧩 Build your own</h4>
Write a few lines of plain English and Aeri learns <b>your</b> routine, your apps, your folders. No code, no build step. Snap the built-in abilities together like blocks, or install one of 13 ready-made plugins.
</td>
<td width="33%" valign="top">
<h4>🪶 Light</h4>
About <b>60 MB</b> of memory and under 0.2% CPU when idle. One Rust process, 27 MB download. Leave it on all day.
</td>
</tr>
</table>

Open source (MIT). Bring any AI model, or run it fully offline with Ollama. The wake word is detected on your PC, and nothing is recorded or sent before you say it. No account, no telemetry.

## You say it, Aeri does it

> **"Rename this file to weekly report"**
> Renames the file selected in Explorer and keeps the extension. No F2, no typing.

> **"Translate this paragraph"**
> Reads what you're looking at, translates it, and puts the result on your clipboard.

> **"Shut down in 30 minutes"**
> Warns you about unsaved work first and only sets it after you confirm. Changed your mind? "Cancel that."

> **"Next video" … "again"**
> One sentence, one action, while your hands are busy.

> **"Compare these three GPUs and write me a summary"**
> Long jobs go to a background agent (Codex or Claude Code). Keep talking meanwhile; Aeri tells you when it's done.

> **"Remind me to drink water in 10 minutes"**
> It speaks up on time, and reminders survive a restart.

> **"Add this to my Obsidian daily note"**
> With a plugin, Aeri uses the app's own link or local API instead of clicking around. Two steps, a few seconds.

## Screenshots

| Ask, and it acts | Settings | Voice, your way |
|:---:|:---:|:---:|
| ![Chat](docs/screenshots/en-chat.png) | ![Settings](docs/screenshots/en-settings.png) | ![Voice settings](docs/screenshots/en-voice.png) |
| Reminders, math, disk space, translation, each with the tool it used | One page per area; problems show in amber | Standard mode with free offline recognition, or realtime voice |

## Why it feels light

| | Measured |
|---|---|
| Idle memory | about **60 MB** |
| Idle CPU | **under 0.2%** of the whole machine |
| Download | **27 MB**, unzip and run, no admin rights |
| Local actions | under **1 ms** (excluding network and model thinking) |
| In-app update | about **9 s** |

One Rust process, no browser engine, no GPU use. The wake word runs locally; audio leaves your PC only after you wake Aeri, and only to the provider you chose.

## Quick start

1. Download `Aeri-vX.Y.Z-windows-x64.zip` from [Releases](https://github.com/yuqtai/Aeri/releases/latest) and unzip it.
2. Run `aeri.exe`.
3. Open **Settings → Language model**, paste an **OpenAI API key**, and click **Save**. That is the only key you need: speech recognition runs locally on your PC (the model downloads once, the first time), and replies are spoken with Windows voices.
4. Say **"Hey Aeri"**.

Prefer another provider? Aeri also works with Anthropic, Google Gemini, OpenRouter, Groq, DeepSeek, a local Ollama model, or any OpenAI-compatible endpoint. Pick it in **Settings → Language model**.

Voice is just as open. Speech recognition: built-in offline models (Moonshine, Whisper, SenseVoice, Parakeet), any local server with the OpenAI transcription API (Qwen3-ASR, faster-whisper…), or OpenAI, Groq, Deepgram, ElevenLabs, Azure, Gemini. Speech output: Windows voices, or OpenAI, ElevenLabs, Azure. Optional realtime voice: OpenAI or Alibaba DashScope.

New versions install from inside the app (Settings → Updates).

## Build your own

Aeri comes with the building blocks: open and close apps, files and folders, volume, brightness, Wi-Fi, dark mode, windows, screenshots, reminders, clipboard, calculator, and local app APIs. A **plugin** is a short note in plain English that snaps these blocks together for *your* PC.

For example, save this as `%APPDATA%\VoiceAssistant\plugins\work-mode\PLUGIN.md`:

```markdown
---
id: work-mode
name: Work mode
version: 1.0.0
description: My morning setup
keywords: work mode, start work
---
When I say "work mode":
1. Open Slack, Outlook and VS Code.
2. Open my project folder D:\code\shop in VS Code.
3. Set the volume to 20 and switch to dark mode.
4. Tell me which reminders I have today.
```

Now say **"Hey Aeri, work mode"** and your morning sets itself up. It works right away: no code, no restart. Write one for your game launcher, your stream setup, your note app, the folder you open fifty times a day.

Want something ready-made? 13 plugins (Spotify, Outlook, VS Code, Slack, Notion, Obsidian…) install in one click from **Settings → Plugins**, or just say "install the Spotify plugin". Built something others would like? Share it in the [plugin market](https://github.com/yuqtai/aeri-plugins).

## Privacy

- The wake word is detected on your PC. Nothing is sent before you wake Aeri.
- After that, audio and text go only to the providers you configured, with your own keys.
- No telemetry. Conversation history stays on your PC for 30 days, and you can turn it off or clear it.
- Plugins can only call local APIs on `127.0.0.1`. App tokens they need are read on your PC and never sent to the model.

## Docs

- [User guide](docs/guide.md): setup, voice modes, providers, settings, history, and phone control
- [Performance](docs/performance.md): architecture and versioned measurements
- [Safety and limitations](docs/safety.md): access, privacy, and open verification items
- [Development](docs/development.md): architecture, builds, translations, and releases
- [Plugins](docs/plugins.md): installation, market, and app guides
- [State machines](docs/state-machines.md): lifecycle and audio routing

[中文文档](docs/zh-CN/guide.md)

## License

[MIT](LICENSE)
