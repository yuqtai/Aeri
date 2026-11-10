# State machines

This inventory reflects Aeri 0.4.0. [简体中文](zh-CN/state-machines.md)

Use an `enum` when a feature has several dependent phases rather than unrelated booleans that allow impossible combinations. Document transitions beside the type and keep state-changing operations together. Independent two-state flags and simple caches do not need a larger state machine.

## Explicit states

| Machine | Source | Representation and transitions |
|---|---|---|
| Assistant | `assistant::Status` | `Idle=0`, `Listening=1`, `Thinking=2`, `Acting=3`, `Speaking=4`, `Paused=5`; shared display contract for Slint, orb, phone |
| Microphone routing | `audio::Mode` | `Idle → Capture → Muted → Barge`; `Barge → Capture` on wake interruption; end returns to `Idle` or `Paused` |
| Phone push-to-talk | `audio::PhoneTurn` plus independent `held` | `None → Requested → Running → None` if released, or `Served` if still held; prevents repeated turns from one press |
| Orb pointer | `ball::Pointer` | `Up → Pressed → Dragging → Up`; release while only pressed is a click |
| Phone server | `remote::Phase` | `Off → Starting → Listening` or `Failed(reason)`; generation guards reject late events from replaced servers |
| Background task | `tasks::State` | `Running → Done / Failed / Cancelled`; completed results can be persisted to history |
| History deletion | `history-confirm` in Slint | Normal → confirmation pending → normal; timeout after three seconds |

`audio::Mode` describes where microphone samples go, not what the UI displays. `Muted` drops samples; `Barge` runs wake-word detection while speaking. The separate phone `held` flag allows a short press to finish a buffered turn even if release occurred before recognition started.

## Deliberately simple representations

| State | Location | Reason |
|---|---|---|
| Update action | `UPDATE_CHECK / INSTALL / BUSY` in `main.rs` | Three integer values shared with Slint, centralized in `wire_update` |
| Unsaved settings | `settings-dirty`, `leave-pending`, `settings-note` in Slint | Different facts: pending edits, attempted navigation, result message |
| Orb visibility | `enabled`, `visible`, `shown` in `ball.rs` | `shown` caches the result of requested visibility and foreground full-screen state in `apply_visibility` |
| Browser session | `Option<Session>` in `tools/browser.rs` | Present or absent |
| Desktop snapshot | `Option<String>` in `tools/desktop.rs` | Cache, not a multiphase operation |

## When changing transitions

Read the transition comments in [audio.rs](../src/audio.rs) before changing microphone or phone behavior. Retain coverage from `phone_press_starts_at_most_one_turn` and `phone_release_before_turn_still_runs_it`; run the suite through `build.ps1 -Test` as described in [development](development.md). Standard/local recognition and optional realtime share these routing states; do not assume `Capture` always means cloud upload.

Persisted history is not a new task state: it records results for viewing and search across restarts. Active task handles and continuation context remain session-local.

[← README](../README.md)
