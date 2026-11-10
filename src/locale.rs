//! Interface language. Aeri is English-first; Chinese is used when the Windows UI language is Chinese,
//! unless the user picks a language in Settings → Appearance. Everything that depends on language
//! (UI text, default providers, default wake word, quick commands, search sites) asks this module.

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    Zh,
}

/// 0 = not decided yet, 1 = English, 2 = Chinese
static LANG: AtomicU8 = AtomicU8::new(0);

/// The language Aeri runs in. Until `apply_preference` is called it follows the environment variable
/// `AERI_LANG=en|zh` (handy for tests) or the Windows UI language.
pub fn lang() -> Lang {
    match LANG.load(Ordering::Relaxed) {
        1 => Lang::En,
        2 => Lang::Zh,
        _ => {
            let l = automatic();
            store(l);
            l
        }
    }
}

pub fn is_zh() -> bool {
    lang() == Lang::Zh
}

/// Pick the text for the current language.
pub fn pick<'a>(en: &'a str, zh: &'a str) -> &'a str {
    if is_zh() { zh } else { en }
}

/// Apply the setting from config.json: "system" (default), "en" or "zh".
/// `AERI_LANG` still wins so tests can force a language.
pub fn apply_preference(pref: &str) {
    let l = if std::env::var("AERI_LANG").is_ok() {
        automatic()
    } else {
        match pref {
            "en" => Lang::En,
            "zh" => Lang::Zh,
            _ => system(),
        }
    };
    store(l);
}

/// The values the language setting can take, in the order shown in Settings
pub const PREFERENCES: [&str; 3] = ["system", "en", "zh"];

fn store(l: Lang) {
    LANG.store(if l == Lang::Zh { 2 } else { 1 }, Ordering::Relaxed);
}

fn automatic() -> Lang {
    match std::env::var("AERI_LANG") {
        Ok(v) => parse(&v),
        Err(_) => system(),
    }
}

fn parse(v: &str) -> Lang {
    if v.trim().to_ascii_lowercase().starts_with("zh") { Lang::Zh } else { Lang::En }
}

fn system() -> Lang {
    // LANGID: the low 10 bits are the primary language; 0x04 is Chinese
    let id = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
    if id & 0x3ff == 0x04 { Lang::Zh } else { Lang::En }
}

/// BCP-47 code for the current language (used by the history page)
pub fn code() -> &'static str {
    match lang() {
        Lang::En => "en",
        Lang::Zh => "zh-CN",
    }
}

/// Name of the bundled Slint translation for the current language ("" = the English source text)
pub fn slint_bundle() -> &'static str {
    if is_zh() { "zh_CN" } else { "" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_override() {
        assert_eq!(parse("zh-CN"), Lang::Zh);
        assert_eq!(parse("ZH"), Lang::Zh);
        assert_eq!(parse("en-US"), Lang::En);
        assert_eq!(parse("fr"), Lang::En);
    }
}
