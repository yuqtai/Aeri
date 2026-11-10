# Plugins

Applies to Aeri 0.4.0. [简体中文](zh-CN/plugins.md)

A plugin is a Markdown guide for using a particular app. It tells the model which tools, local APIs, or app links to use. It does not load executable plugin code. General tools for files, apps, volume, screenshots, reminders, and power are built in.

## Install and use

Open **Settings → Plugins** to browse, install, update, or uninstall a guide. You can also ask Aeri to install a plugin by name. Notepad is the only bundled guide in 0.4.0; other apps such as Obsidian and SiYuan come from the market. Bundled guides cannot be uninstalled, but a market update can override them.

Aeri attaches relevant guides when your request mentions the plugin name/keywords or the foreground app matches its process name. Explicit name/keyword matches rank first; at most two guides are attached automatically. You can ask for a guide through `plugin_guide` if it was not selected.

## Market and files

The market is [yuqtai/aeri-plugins](https://github.com/yuqtai/aeri-plugins). Its `index.json` lists versions and SHA-256 hashes. Aeri verifies a downloaded guide against the index before installation. Installed guides live under `%APPDATA%\VoiceAssistant\plugins`.

For contributions, follow that repository's README and submit a pull request there. SHA-256 checks detect mismatched downloads; they do not make an untrusted guide safe.

## Local APIs and secrets

`local_http` accepts HTTP(S) requests to loopback hosts (`127.0.0.1`, `localhost`, `::1`) only. A guide can use `{{secret:file#JSON-pointer}}` placeholders for app tokens. Aeri reads permitted files under the user profile, substitutes the values locally, and masks those values in returned text before it reaches the model. Do not paste raw tokens into a guide.

Guides remain model instructions and can lead to actions through Aeri's tools. Review sources and install only trusted guides; manually copying unknown Markdown into the plugin directory bypasses market download checks. See [safety](safety.md).

## Implementation

- [src/plugins.rs](../src/plugins.rs) contains parsing, matching, market downloads, and the `plugin_guide`, `local_http`, and `plugin_market` tools.
- Sources live in [plugins/](../plugins/). Only entries in `BUILTIN` are embedded through `include_str!`.
- After changing a market guide, sync it to the market repository and regenerate its index with that repository's `make_index.py`.

[← README](../README.md)
