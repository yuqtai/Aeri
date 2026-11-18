# User guide

Applies to Aeri 0.4.9. [简体中文](zh-CN/guide.md)

## First run

1. Download the Windows x64 ZIP from [Releases](https://github.com/yuqtai/Aeri/releases/latest), extract the `Aeri` folder, and run `aeri.exe`. First run (or missing required keys / post-update restart) opens the main window automatically; launching `aeri.exe` again while running brings the existing window to the front.
2. Open **Settings → Language model**, choose your provider, enter its key, and save and apply. English installs start with OpenAI; Chinese installs start with DeepSeek. Ollama can use a local model without an API key.
3. Leave **Voice chat → Mode** on **Standard (recommended)** and **Speech recognition** on **Local (offline)**. Use **Test speech recognition** to download and load the model before your first conversation.
4. Say **“Hey Aeri”** on an English install, or **“你好电脑”** on a Chinese install. You can also click the orb, press Ctrl+Alt+Space, or type a request.

Local recognition downloads the selected sherpa-onnx model on first use, verifies SHA-256, and stores it under `%APPDATA%\VoiceAssistant\models`. This first download needs internet access and Windows `tar.exe`; recognition then works offline. Standard mode speaks through Windows SAPI by default (or a configured cloud/custom voice). Language-model requests still need the configured provider unless you use a local endpoint.

Aeri follows the Windows **display language** unless you choose a specific language in **Settings → Appearance & startup → Language** (which switches immediately). `AERI_LANG=en` or `AERI_LANG=zh` overrides this at process startup. These choices affect new-install defaults, including the wake word; changing language or upgrading does not replace values already saved in `config.json`.

## Window and floating orb

- Click the floating orb to open the main window beside it (sized and clamped to fit your monitor's work area and DPI scaling). Closing the window returns to the orb; use **Quit** in the orb or tray menu to exit.
- Clicks on the soft outer halo outside the orb body pass straight through (`HTTRANSPARENT`) to underlying windows without blocking your work.
- Drag the orb to move it across monitors (including mixed-DPI setups). It snaps near screen edges, remembers its position, and hides automatically only when the topmost visible window **on the same monitor** is full screen (Do Not Disturb mode or full-screen video on another monitor will not hide it).
- Right-click for talk/stop, wake-word pause/resume, hide, and quit actions. The tray menu dynamically toggles **Show floating orb** / **Hide floating orb**; hiding the orb while the main window is closed opens the window with a reminder of where to show it again. A green dot indicates a background task.
- The main listening orb shows idle, listening, thinking, acting, speaking, or paused state. Click it while busy to stop the current turn.
- Chat shows your requests, replies (with a one-click **Copy** button), tool activity, and background progress. Clearing the chat uses a two-click confirmation in the top bar. The lower-right control opens text input; the lower-left switch pauses voice wake.

## Settings

There are nine categories. Less frequently changed fields are under **Advanced options**. Changes take effect after **Save & apply**; leaving with unsaved changes offers save or discard, and closing the window reverts any unsaved theme preview. Restore defaults preserves saved keys.

| Category | Controls |
|---|---|
| Voice chat | Standard/realtime mode cards, speech recognition (local models or cloud/custom) and test, speech output voice/preview, speaking rate, follow-up listening, interruption mode (talk / wake word / never), headphone listen-while-speaking, lower other audio; advanced fallback ASR test, silence and speech timeout |
| Wake & microphone | Microphone selection/refresh (retains temporarily disconnected external mics), wake words (validated on save), sensitivity, wake on startup, Ctrl+Alt+Space; advanced wake-word weight |
| Language model | Provider, key (with eye toggle)/test/clear, model, thinking and effort; advanced endpoint and new-conversation timeout |
| Computer control | Desktop and browser access, Codex/Claude Code/built-in executor, Full access/Working directory/Read only (enforced across all three agents); advanced working directory, agent model, browser idle timeout, Playwright package |
| Phone control | Enable LAN control, pair phone, unpair all phones (two-click confirmation); `remote_port` in config defaults to 47821 |
| Appearance & startup | System/light/dark theme, interface language, floating orb, start with Windows, open window on startup |
| Software updates | Check/install updates, daily automatic checks |
| History | View/search, save history, clear saved chats and task records |
| Plugins | Browse, install, update, uninstall app guides |

Settings are stored in `%APPDATA%\VoiceAssistant\config.json`; manual editing is normally unnecessary. Use a distinctive wake phrase. Unsupported words or characters are caught when saving; Aeri uses an English pronunciation lexicon (`en.phone`) and Chinese pinyin tokens.

## Providers and keys

These are the presets shipped in 0.4.9, not a guarantee that a provider still offers a model. Change the model or service URL in settings if your account needs a different one.

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

| Voice path | Providers in 0.4.9 |
|---|---|
| Standard recognition | Local sherpa-onnx (Zipformer, Moonshine, Whisper, SenseVoice, Parakeet); OpenAI (`gpt-transcribe`); Groq (`whisper-large-v3-turbo`); Deepgram (`nova-3`); ElevenLabs (`scribe_v2`); Azure AI Speech; Google Gemini; Custom (OpenAI transcription over `http(s)://` or Alibaba/Qwen streaming over `wss://`) |
| Standard speech output | Windows SAPI (offline); OpenAI; ElevenLabs; Azure AI Speech; Custom (OpenAI speech over `http(s)://` or Alibaba CosyVoice over `wss://`), with automatic Windows SAPI fallback |
| Optional realtime | OpenAI (`gpt-realtime-2`, `marin`), Azure OpenAI, xAI Grok, or Custom (OpenAI Realtime-compatible or Alibaba DashScope / Qwen realtime) |

Realtime sends audio to its provider for transcription and speech. With an LLM key configured, your selected language model handles reasoning and tools; otherwise the realtime model can handle them. Typed requests use the Standard path. Realtime connection failure can fall back to Standard recognition.

Speech and LLM keys for the same provider (such as OpenAI or Gemini) can be shared automatically, or entered per section; saved keys take priority over environment fallbacks and can be inspected in Settings with the eye button (older DPAPI-encrypted keys are migrated on load).

Codex and Claude Code use their own CLI installations and login sessions. Install and sign in to the chosen CLI separately; the built-in executor uses Aeri's selected language model.

## History and longer tasks

History is enabled by default. Chats and completed task records are kept for 30 days under `%APPDATA%\VoiceAssistant\history\YYYY-MM-DD.jsonl`. Startup restores recent chat messages for display. **History → View** opens a searchable local page. The current pruning code runs at startup and uses a 30-day window relative to the newest saved day, so an inactive installation can retain older records until a later cleanup. Turning saving off stops new records; use **Clear** and then **Confirm clear** to delete existing records (confirmation expires after three seconds).

Long jobs can run in the background while you keep talking. Only one background task runs at a time. In the current session, ask to continue or retry a task with its earlier context. Saved results can be searched after restart; they do not restore a running task or its in-memory continuation state. Reminders are stored separately in `reminders.json` and survive restarts.

## Phone control (Android app)

The Android app connects directly to the PC on the same LAN. It supports push-to-talk, typed requests, stop, wake pause/resume, volume, mute, and media controls. Replies play on the PC. A PC microphone is not required for phone speech input.

1. Enable **Settings → Phone control**, save, and when Windows Firewall asks, allow both **Private and Public networks** (Windows often marks home Wi-Fi as Public by default; Aeri still accepts only local/LAN addresses).
2. Install `android\Aeri.apk` beside `aeri.exe` (or `Aeri-vX.Y.Z-android.apk` from Releases). Use Android 8 or newer and the same Wi-Fi network.
3. Click **Pair a new phone** on the PC and enter its six-digit code on the phone.

The app discovers the PC on the LAN and remembers pairing. **Unpair** (two-click confirmation) disconnects phones and requires pairing again. The app disconnects in the background. When a newer APK is bundled with the PC, the phone offers a local transfer, verification, and the Android installation prompt. Keep the signing certificate consistent across updates. See the [remote protocol](../android/PROTOCOL.md).

LAN transport is not encrypted. Enable it only on a trusted network; see [safety and limitations](safety.md).

[← README](../README.md)
