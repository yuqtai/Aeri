---
id: spotify
name: Spotify
version: 1.0.0
description: Open music and searches in Spotify and control media playback
match: Spotify.exe
keywords: spotify, music, 音乐, 声田
author: Aeri
---
Use open_url with Spotify links. URL-encode search text (spaces as %20, Chinese as UTF-8 percent escapes); never invent resource IDs.

- **Search:** `spotify:search:QUERY`. Results appear in Spotify; the link does not select or play a result. Ask the user to choose if the desired recording is ambiguous.
- **Open music:** Use a supplied Spotify URI, such as `spotify:track:TRACK_ID`, `spotify:album:ALBUM_ID` or `spotify:artist:ARTIST_ID`, or the original Spotify share link. Opening a resource is not proof that playback started.
- **Playback:** Use media_control with `{"action":"play_pause","times":1}`, `{"action":"next","times":1}` or `{"action":"previous","times":1}`. These are system media keys, not Spotify-specific commands; use only when Spotify is the intended active media session. play_pause toggles, so do not repeat blindly or claim an unobserved playback state.
- If the link fails, report it; do not guess controls in an inaccessible interface or call Spotify's cloud API through local_http.
- Purchases or subscriptions require explicit user confirmation first. Do not transmit data through web searches or external services; use only Spotify's own links for navigation.

open_url confirms dispatch only. Tell the user which music or search was opened and leave result selection to them when it cannot be verified.
