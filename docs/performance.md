# Performance

Applies to the architecture in 0.4.0. The measurements below are historical, not a new 0.4.0 benchmark. [简体中文](zh-CN/performance.md)

## What affects resource use

The Windows app uses Rust and Slint software rendering. Idle UI animations stop. Wake-word detection runs locally behind an energy gate. `agent-desktop` starts for desktop operations; Codex, Claude Code, and browser automation start when needed rather than remaining resident.

Standard mode now defaults to **local speech recognition**. Its model is downloaded separately on first use and is not included in the app ZIP. Loading recognition, running a local LLM, browser automation, or a background agent changes memory and CPU use. The older idle figures below do not measure this new recognition path.

Local fast-path rules in Standard mode handle supported common commands without an LLM call. Dedicated tools handle files, volume, windows, screenshots, reminders, calculation, and system information. Other requests include recognition, network latency, model reasoning, tools, and speech synthesis. Realtime mode uses its provider for listening and speech, and generally uses the configured LLM for reasoning; it does not use the spoken fast-path rules.

## Recorded measurements

Source: the project's pre-pivot performance notes. Test PC: AMD Ryzen 5 5600 (6 cores/12 threads), 16 GB RAM, Windows 11 Pro. Idle measurements used **Aeri 0.3.1 release**, floating orb only, wake enabled, for five minutes.

| Idle metric (0.3.1) | Recorded result |
|---|---|
| Private memory | 61–62 MB; working set about 105 MB |
| CPU | 1–2% of one core; about 0.1–0.2% of the whole machine |
| Resident app processes | 1; desktop helper starts temporarily |

The following timings were recorded in the same 0.3.1 test notes (median of three calls, excluding network/model time). The update timing includes download and restart.

| Operation | Recorded time |
|---|---|
| Calculation, time, app lookup, file information, PC status | Under 1 ms |
| Read current window text (`screen_read`) | About 46 ms |
| List windows (desktop helper) | About 175 ms |
| File-name search using Windows Search index | About 0.6 s |
| Full controls snapshot (editor window) | About 0.7 s |
| MCP process startup to first response | 27 ms |
| In-app update (22 MB download, verify, replace, restart) | About 9 s |

Package sizes were measured separately on **0.3.5**: 24.2 MB ZIP, 55.6 MB extracted (24.4 MB executable, 3.3 MB desktop helper, 21.4 MB DLLs, 5.2 MB wake model, 1.3 MB Android app). These are not 0.4.0 download or installed-size claims.

## Rechecking on 0.4.0

Record the version, hardware, Windows version, voice mode/provider, loaded models, and whether background agents/browser processes are included. Measure idle separately from first model download/load, recognition, and active tasks. Use several runs and report network/model time separately from local tool time. No fresh 0.4.0 resource or latency measurements are supplied here.

[User guide](guide.md) · [← README](../README.md)
