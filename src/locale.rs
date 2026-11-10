#![allow(dead_code)] // used by the language-aware parts as they are converted
//! Interface language. Aeri is English-first; Chinese is used when the Windows UI language is Chinese.
//! Everything that depends on language (UI text, default providers, default wake word, quick commands,
//! search sites) should ask this module instead of checking the system itself.

use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    Zh,
}

static LANG: OnceLock<Lang> = OnceLock::new();

/// The language Aeri runs in. Decided once per process from the Windows UI language;
/// the environment variable `AERI_LANG=en|zh` overrides it (handy for testing).
pub fn lang() -> Lang {
    *LANG.get_or_init(|| {
        if let Ok(v) = std::env::var("AERI_LANG") {
            return parse(&v);
        }
        system()
    })
}

pub fn is_zh() -> bool {
    lang() == Lang::Zh
}

/// Pick the text for the current language.
pub fn pick<'a>(en: &'a str, zh: &'a str) -> &'a str {
    if is_zh() { zh } else { en }
}

fn parse(v: &str) -> Lang {
    if v.trim().to_ascii_lowercase().starts_with("zh") { Lang::Zh } else { Lang::En }
}

fn system() -> Lang {
    // LANGID: the low 10 bits are the primary language; 0x04 is Chinese
    let id = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
    if id & 0x3ff == 0x04 { Lang::Zh } else { Lang::En }
}

/// BCP-47 code for the current language (used by Slint translations and web search URLs)
pub fn code() -> &'static str {
    match lang() {
        Lang::En => "en",
        Lang::Zh => "zh-CN",
    }
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
