# User guide

Applies to Aeri 0.4.0. [简体中文](zh-CN/guide.md)

## First run

1. Download the Windows x64 ZIP from [Releases](https://github.com/yuqtai/Aeri/releases/latest), extract it, and run `aeri.exe`.
2. Open **Settings → Language model**, choose your provider, enter its key, and save and apply. English installs start with OpenAI; Chinese installs start with DeepSeek. Ollama can use a local model without an API key.
3. Leave **Voice chat → Conversation mode** on **Standard (recommended)** and **Speech recognition** on **Local (offline)**. Use **Test speech recognition** to download and load the model before your first conversation.
4. Say **“Hey Aeri”** on an English install, or **“你好电脑”** on a Chinese install. You can also click the orb, press Ctrl+Alt+Space, or type a request.

Local recognition downloads the appropriate English or Chinese sherpa-onnx model on first use, verifies SHA-256, and stores it under `%APPDATA%\VoiceAssistant\models`. This first download needs internet access and Windows `tar.exe`; recognition then works offline. Standard mode speaks through Windows SAPI. Language-model requests still need the configured provider unless you use a local endpoint.

Aeri follows the Windows **display language**: Chinese UI language selects Chinese; other languages select English. `AERI_LANG=en` or `AERI_LANG=zh` overrides this at process startup. These choices affect new-install defaults, including the wake word; changing language or upgrading does not replace values already saved in `config.json`.

## Window and floating orb

- Click the floating orb to open the main window. Closing the window returns to the orb; use **Quit** in the orb or tray menu to exit.
- Drag the orb to move it. It snaps near screen edges, remembers its position, and hides while the foreground app is full screen.
- Right-click for talk/stop, wake-word pause/resume, hide/show, and quit actions. A green dot indicates a background task.
- The main listening orb shows idle, listening, thinking, acting, speaking, or paused state. Click it while busy to stop the current turn.
- Chat shows your requests, replies, tool activity, and background progress. The lower-right control opens text input; the lower-left switch pauses voice wake.

## Settings

There are nine categories. Less frequently changed fields are under **Advanced options**. Changes take effect after **Save & apply**; leaving with unsaved changes offers save or discard. Restore defaults preserves saved keys.

| Category | Controls |
|---|---|
| Voice chat | Standard/realtime mode, speech provider and test, speaking rate, follow-up listening, wake-word interruption, headphone interruption, lower other audio; advanced model, URL, voice, silence and speech timeout |
| Wake & microphone | Microphone selection/refresh, wake words, sensitivity, wake on startup, Ctrl+Alt+Space; advanced wake-word weight |
| Language model | Provider, key/test/clear, model, thinking and effort; advanced endpoint and new-conversation timeout |
| Computer control | Desktop and browser access, Codex/Claude Code/built-in executor, Full access/Working directory/Read only; advanced working directory, agent model, browser idle timeout, Playwright package |
| Phone control | Enable LAN control, pair phone, revoke pairings; `remote_port` in config defaults to 47821 |
| Appearance & startup | System/light/dark theme, floating orb, start with Windows, open window on startup |
| Software updates | Check/install updates, daily automatic checks |
| History | View/search, save history, clear saved chats and task records |
| Plugins | Browse, install, update, uninstall app guides |

Settings are stored in `%APPDATA%\VoiceAssistant\config.json`; manual editing is normally unnecessary. Use a distinctive wake phrase. Unsupported English words produce a pronunciation warning; Aeri uses an English pronunciation lexicon and Chinese pinyin tokens.

## Providers and keys

These are the presets shipped in 0.4.0, not a guarantee that a provider still offers a model. Change the model or service URL in settings if your account needs a different one.

| Language model | Preset model | Environment fallback |
|---|---|---|
| OpenAI | `gpt-4.1-mini` | `OPENAI_API_KEY` |
| Anthropic | `claude-sonnet-5-5` | `ANTHROPIC_API_KEY` |
| Google Gemini | `gemini-3.8-flash` | `GEMINI_API_KEY` |
| OpenRouter | `openai/gpt-4.1-mini` | `OPENROUTER_API_KEY` |
| Groq | `openai/gpt-oss-120b` | `GROQ_API_KEY` |
| DeepSeek | `deepseek-flash` | `DEEPSEEK_API_KEY` |
| Ollama | `qwen3`, `http://localhost:11434/v1` | No key required |
| Custom | `default`, `http://localhost:8000/v1` | Configure endpoint/model; key optional |

OpenAI-compatible providers use Chat Completions; Anthropic and the DeepSeek preset use the Anthropic message format.

| Voice path | Providers in 0.4.0 |
|---|---|
| Standard recognition | Local sherpa-onnx; OpenAI (`gpt-transcribe`); Groq (`whisper-large-v3-turbo`); Alibaba DashScope (`fun-asr-realtime`) |
| Standard speech output | Installed Windows SAPI voices; prefers a matching English/Chinese voice, then Natural/Neural if exposed through SAPI |
| Optional realtime | OpenAI (`gpt-realtime-2`, `marin`) or Alibaba DashScope (`qwen-audio-3.1-realtime-plus`, `longanqian_v3.1`) |

Realtime sends audio to its provider for transcription and speech. With an LLM key configured, your selected language model handles reasoning and tools; otherwise the realtime model can handle them. Typed requests use the Standard path. Realtime connection failure can fall back to Standard recognition.

OpenAI recognition and realtime can share the saved OpenAI LLM key or `OPENAI_API_KEY`; an explicit voice key takes priority. Groq recognition can use `GROQ_API_KEY`. DashScope uses `DASHSCOPE_API_KEY`. Saved keys take priority over environment fallbacks and are protected with Windows DPAPI for the current Windows account.

Codex and Claude Code use their own CLI installations and login sessions. Install and sign in to the chosen CLI separately; the built-in executor uses Aeri's selected language model.

## History and longer tasks

History is enabled by default. Chats and completed task records are kept for 30 days under `%APPDATA%\VoiceAssistant\history\YYYY-MM-DD.jsonl`. Startup restores recent chat messages for display. **History → View** opens a searchable local page. The current pruning code runs at startup and uses a 30-day window relative to the newest saved day, so an inactive installation can retain older records until a later cleanup. Turning saving off stops new records; use **Clear** and then **Confirm clear** to delete existing records (confirmation expires after three seconds).

Long jobs can run in the background while you keep talking. Only one background task runs at a time. In the current session, ask to continue or retry a task with its earlier context. Saved results can be searched after restart; they do not restore a running task or its in-memory continuation state. Reminders are stored separately in `reminders.json` and survive restarts.

## Phone control (Android app)

The Android app connects directly to the PC on the same LAN. It supports push-to-talk, typed requests, stop, wake pause/resume, volume, mute, and media controls. Replies play on the PC. A PC microphone is not required for phone speech input.

1. Enable **Settings → Phone control**, save, and allow Windows Firewall access on your private network.
2. Install `Aeri-vX.Y.Z-android.apk` from Releases, or `android\Aeri.apk` beside the PC executable. Use Android 8 or newer and the same Wi-Fi network.
3. Click **Pair new phone** on the PC and enter its six-digit code on the phone.

The app discovers the PC on the LAN and remembers pairing. **Revoke all pairings** disconnects phones and requires pairing again. The app disconnects in the background. When a newer APK is bundled with the PC, the phone offers a local transfer, verification, and the Android installation prompt. Keep the signing certificate consistent across updates. See the [remote protocol](../android/PROTOCOL.md).

LAN transport is not encrypted. Enable it only on a trusted network; see [safety and limitations](safety.md).

[← README](../README.md)
