# Development, builds, and releases

Applies to source version 0.4.0. [简体中文](zh-CN/development.md)

## Architecture

```text
Microphone → 16 kHz resampling → energy gate → local wake-word detector
  → Standard (new-install default): local/cloud ASR → fast path or selected LLM → Windows SAPI
  → Optional realtime: OpenAI/DashScope audio → selected LLM/tools → realtime speech
  → Long jobs: Codex / Claude Code / built-in executor → MCP tools → reported result
```

Wake detection is local sherpa-onnx KWS. Standard recognition defaults to sherpa-onnx offline ASR, with a separately downloaded, hash-checked model on first use. Realtime streams audio to the selected provider; OpenAI input is resampled to 24 kHz. Speech playback can be stopped. During speech, wake-only interruption avoids feeding speaker audio back into recognition; headphone interruption can keep full capture on.

Other apps' audio is normally lowered to 20% during conversation and restored afterwards. Sessions manually adjusted by the user are not restored over their changes. Recovery data supports restoration after a crash.

| Files | Responsibility |
|---|---|
| `src/main.rs`, `ui/app.slint` | Slint UI, tray, settings wiring; `--mcp`, `--kws-test` entry points |
| `src/locale.rs`, `ui/zh_CN.po`, `build.rs` | Windows display-language detection, Rust copy, bundled Slint translations |
| `src/config.rs` | New-install versus legacy defaults, saved configuration, DPAPI keys |
| `src/assistant.rs`, `src/audio.rs`, `src/kws.rs`, `src/resample.rs` | Conversation scheduling, microphone routing, wake detection, resampling |
| `src/asr.rs`, `src/tts.rs` | Local/OpenAI/Groq/DashScope recognition; Windows SAPI output |
| `src/realtime.rs`, `src/player.rs` | OpenAI/DashScope WebSocket sessions and interruptible 24 kHz output |
| `src/llm.rs`, `src/agent.rs`, `src/fastpath.rs` | Provider presets/clients, tool loop, local command rules |
| `src/tasks.rs`, `src/mcp.rs` | Background executors and stdio JSON-RPC tools |
| `src/tools/`, `src/plugins.rs`, `plugins/` | Local tools, desktop/browser adapters, app guides and market |
| `src/history.rs`, `src/reminders.rs` | 30-day local chat/task records; persistent reminders |
| `src/ball.rs`, `src/sys.rs`, `src/screen.rs` | Win32 floating orb, system integration, foreground context |
| `src/remote.rs`, `android/` | LAN phone service and Kotlin/Compose client; [protocol](../android/PROTOCOL.md) |
| `src/update.rs` | GitHub update download, verification, replacement/rollback, restart |

See [state machines](state-machines.md) before changing routing or lifecycle behavior.

## Windows prerequisites

Install Rust through rustup and Visual Studio 2022 Build Tools with C++ tools and a Windows SDK. Use **build.ps1**, which loads the MSVC environment and fills in SDK paths; plain Cargo from an unconfigured shell can fail at link time.

The wake model is not tracked in Git. Download `sherpa-onnx-kws-zipformer-zh-en-3M-2025-12-20.tar.bz2` from the upstream [KWS models release](https://github.com/k2-fsa/sherpa-onnx/releases/tag/kws-models) and put its contents in `models\kws`. Runtime files include:

- `encoder-epoch-13-avg-2-chunk-16-left-64.int8.onnx`
- `decoder-epoch-13-avg-2-chunk-16-left-64.onnx`
- `joiner-epoch-13-avg-2-chunk-16-left-64.int8.onnx`
- `tokens.txt` and the tracked English lexicon `en.phone` (retain this when extracting)

This bundled wake model is distinct from the ASR model downloaded into the user's app-data directory on first use.

## Debug builds and tests

Run from the repository root:

```powershell
powershell -ExecutionPolicy Bypass -File build.ps1 -Test
powershell -ExecutionPolicy Bypass -File build.ps1
```

Check local documentation links with `python docs/check_links.py`. It scans both READMEs and all Markdown under `docs`, including image paths and heading fragments.

By default, output is under `%USERPROFILE%\.cargo-target\va\debug`. An existing `CARGO_TARGET_DIR` is respected. To isolate concurrent work or choose a short path, set it before either command:

```powershell
$env:CARGO_TARGET_DIR = 'C:\aeri-target'
powershell -ExecutionPolicy Bypass -File build.ps1
```

The script downloads pinned `agent-desktop` v1.0.2 on first build, verifies SHA-256, caches it under `vendor`, and copies the helper and runtime models beside the executable. Browser tools separately need Node.js/npx for Playwright MCP. Run the debug executable from its output directory, keeping DLLs/models beside it. `-Run` also starts it; `aeri.exe --kws-test a.wav b.wav` checks wake detection from WAV files.

**`build.ps1 -Release` has installation side effects**: it stages `dist`, can stop a running copy there, and, when an installation marker exists, updates and launches the installed app. Use the debug build for normal development and verification.

## Android

Install JDK 17+ and an Android SDK. Set `ANDROID_HOME`/`ANDROID_SDK_ROOT`, or `sdk.dir` in `android/local.properties`. From `android`, run `./gradlew.bat assembleRelease`. Output: `android/app/build/outputs/apk/release/app-release.apk`.

Use an ASCII-only checkout path for Android builds. Android's build tools may reject non-ASCII paths; changing `CARGO_TARGET_DIR` only moves Rust output and does not fix the Android project path.

`build.ps1 -Android` adds the APK to a debug build; release builds always attempt Android. Missing JDK/SDK produces a warning and skips the app. The script puts `Aeri.apk` and version/size/SHA-256 metadata `Aeri.json` under `android` beside the PC executable. Version name/code come from `Cargo.toml`.

The current Android release type uses `%USERPROFILE%\.android\debug.keystore`. Preserve this certificate when changing build machines: a different signature prevents an in-place Android update. Do not commit the private key.

## Installation and updates

`install.ps1 -Launch` installs staged release files per user under `%LOCALAPPDATA%\Programs\VoiceAssistant`, creates shortcuts and an uninstall entry, and needs no admin rights. Settings, keys, reminders, plugins, and history remain under `%APPDATA%\VoiceAssistant`. Installation updates and uninstall preserve that user data.

Startup uses the current-user Run registry entry. Moving the executable requires launching it once to update its registered path. Autostart normally shows only the orb.

In-app updates check GitHub Releases, verify the asset digest, extract to `%TEMP%\aeri-update`, wait for exit, replace files, roll back on replacement failure, and restart. Daily checks notify; they do not install silently. `aeri.exe --update` starts the command-line update path.

## Maintainer release workflow

This section describes publishing; it is not part of a debug/test run. Commit the intended `Cargo.toml` version and all changes, then a maintainer can run `scripts/release.ps1 -Notes <notes-file>`. It requires a clean worktree, invokes the release build unless `-SkipBuild` is supplied, packages `dist` with third-party notices, writes SHA-256, creates an annotated version tag, **pushes HEAD to main and the tag**, and creates the GitHub Release through `gh`.

Do not invoke it from a feature branch just to test packaging. `-SkipBuild` skips compilation, not publishing. Auto-update expects `Aeri-vX.Y.Z-windows-x64.zip`; the script also uploads its `.sha256` and, if present, `Aeri-vX.Y.Z-android.apk`. Preserve those names.

## UI translations and configuration compatibility

Use concise English `@tr()` strings in Slint, keeping substitutions in placeholders so translators can reorder them. Chinese translations live in `ui/zh_CN.po`. Extract/merge with the matching extractor and GNU gettext:

```powershell
cargo install slint-tr-extractor --version 1.18.1
slint-tr-extractor --no-default-translation-context -d aeri -o ui/aeri.pot ui/app.slint
msgmerge --update --backup=none ui/zh_CN.po ui/aeri.pot
```

Review fuzzy/new entries; commit the Slint source and PO, not the intermediate POT. `build.rs` currently stages `ui/zh_CN.po` as `zh_CN/LC_MESSAGES/aeri.po` in `OUT_DIR` and bundles it without runtime MO files or default component contexts. Adding another catalog requires extending this staging code.

Startup selects the Chinese bundle when `locale::is_zh()` is true; otherwise it uses English. Rust copy uses `locale::pick(en, zh)` and checked formatting through `ui_format!`. The history page gets `locale::code()`. Test both with `AERI_LANG=en` and `AERI_LANG=zh` using isolated app-data settings. Use `listen_on_start=false`, `remote_enabled=false`, and `show_window_on_start=true` for UI review, and account for the single-instance app.

**Never overwrite saved config values to apply new-install defaults.** `Config::new_install` selects Standard/local ASR for both languages, OpenAI for English LLM/realtime, and DeepSeek/DashScope for Chinese. Legacy deserialization keeps older provider/mode values. See the configuration compatibility tests before changing these paths.

## Icons

The brand source is `assets/app.svg`; `assets/app.ico` supplies Windows sizes. `python scripts/generate_icon.py` (Pillow required) generates PNG, ICO, and embedded RGBA assets. Rebuild in debug to inspect changes.

[← README](../README.md)
