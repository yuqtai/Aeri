//! 唤醒词检测：sherpa-onnx 关键词模型（约 3M 参数），唤醒词可随时修改，不需要重新训练。

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, bail};
use pinyin::ToPinyin;
use sherpa_onnx::{KeywordSpotter, KeywordSpotterConfig, OnlineStream};

/// 声母（含 y/w，和 sherpa-onnx 的 ppinyin 切分方式一致）
const INITIALS: [&str; 23] =
    ["zh", "ch", "sh", "b", "p", "m", "f", "d", "t", "n", "l", "g", "k", "h", "j", "q", "x", "r", "z", "c", "s", "y", "w"];

/// Convert Chinese syllables to pinyin tokens and ASCII words to ARPAbet phones.
fn word_tokens(word: &str, lexicon: &HashMap<String, Vec<String>>) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = word.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_alphabetic() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_alphabetic() { i += 1; }
            let english: String = chars[start..i].iter().collect();
            let phones = lexicon.get(&english.to_ascii_uppercase())
                .ok_or_else(|| crate::locale::pick(&format!("Unknown English wake-word token: {english}"), &format!("英文唤醒词词典中没有：{english}")).to_string())?;
            tokens.extend(phones.iter().cloned());
            continue;
        }
        let ch = chars[i];
        i += 1;
        if ch.is_whitespace() { continue; }
        let py = ch.to_pinyin().ok_or_else(|| crate::locale::pick(&format!("Unsupported wake-word character: {ch}"), &format!("不支持的唤醒词字符：{ch}")).to_string())?;
        let syllable = py.with_tone();
        match INITIALS.iter().find(|i| syllable.starts_with(**i) && syllable.len() > i.len()) {
            Some(initial) => {
                tokens.push(initial.to_string());
                tokens.push(syllable[initial.len()..].to_string());
            }
            None => tokens.push(syllable.to_string()),
        }
    }
    if tokens.is_empty() { return Err(crate::locale::pick("The wake word is empty", "唤醒词为空").into()); }
    Ok(tokens)
}

fn english_lexicon(text: &str) -> HashMap<String, Vec<String>> {
    let mut lexicon = HashMap::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        if let Some(word) = fields.next() {
            let phones: Vec<String> = fields.map(str::to_string).collect();
            if !phones.is_empty() { lexicon.entry(word.to_ascii_uppercase()).or_insert(phones); }
        }
    }
    // Aeri is pronounced like "airy" and is absent from the upstream lexicon.
    lexicon.insert("AERI".into(), vec!["EH1".into(), "R".into(), "IY0".into()]);
    lexicon
}

// Sherpa keyword labels cannot contain whitespace; restore the user's phrase on detection.
fn keyword_label(word: &str) -> String {
    word.split_whitespace().collect::<Vec<_>>().join("_")
}

/// 生成 keywords 文本；返回（文本，无法使用的唤醒词说明）
fn keywords_text(words: &[String], vocab: &HashSet<String>, lexicon: &HashMap<String, Vec<String>>) -> (String, Vec<String>) {
    let mut lines = Vec::new();
    let mut problems = Vec::new();
    for word in words.iter().map(|w| w.trim()).filter(|w| !w.is_empty()) {
        match word_tokens(word, lexicon) {
            Ok(tokens) if tokens.iter().all(|t| vocab.contains(t)) => {
                lines.push(format!("{} @{}", tokens.join(" "), keyword_label(word)));
            }
            Ok(_) => problems.push(crate::locale::pick(&format!("Unsupported model pronunciation: {word}"), &format!("“{word}”含有模型不支持的读音")).to_string()),
            Err(problem) => problems.push(format!("{word}: {problem}")),
        }
    }
    (lines.join("\n"), problems)
}

pub struct WakeDetector {
    spotter: KeywordSpotter,
    stream: OnlineStream,
    labels: HashMap<String, String>,
}

impl WakeDetector {
    pub fn new(model_dir: &Path, words: &[String], threshold: f32, score: f32) -> anyhow::Result<(Self, Vec<String>)> {
        let file = |name: &str| model_dir.join(name).to_string_lossy().into_owned();
        let tokens_path = model_dir.join("tokens.txt");
        let vocab: HashSet<String> = std::fs::read_to_string(&tokens_path)
            .with_context(|| crate::locale::pick(&format!("Wake-word model not found: {}", tokens_path.display()), &format!("找不到唤醒词模型：{}", tokens_path.display())).to_string())?
            .lines()
            .filter_map(|l| l.split_whitespace().next().map(str::to_string))
            .collect();
        let lexicon_path = model_dir.join("en.phone");
        let lexicon = english_lexicon(&std::fs::read_to_string(&lexicon_path).unwrap_or_default());
        let (keywords, mut problems) = keywords_text(words, &vocab, &lexicon);
        if !lexicon_path.is_file() {
            problems.push(crate::locale::pick(&format!("English lexicon missing: {}", lexicon_path.display()), &format!("英文唤醒词词典缺失：{}", lexicon_path.display())).to_string());
        }
        if keywords.is_empty() {
            let details = problems.join("; ");
            bail!(crate::locale::pick(&format!("No usable wake words. {details}"), &format!("没有可用的唤醒词。{details}")).to_string());
        }

        let mut config = KeywordSpotterConfig::default();
        config.model_config.transducer.encoder = Some(file("encoder-epoch-13-avg-2-chunk-16-left-64.int8.onnx"));
        config.model_config.transducer.decoder = Some(file("decoder-epoch-13-avg-2-chunk-16-left-64.onnx"));
        config.model_config.transducer.joiner = Some(file("joiner-epoch-13-avg-2-chunk-16-left-64.int8.onnx"));
        config.model_config.tokens = Some(file("tokens.txt"));
        config.model_config.num_threads = 1;
        config.keywords_threshold = threshold;
        config.keywords_score = score;
        config.keywords_buf = Some(keywords);
        let spotter = KeywordSpotter::create(&config).context(crate::locale::pick("Failed to load wake-word model", "加载唤醒词模型失败"))?;
        let stream = spotter.create_stream();
        let labels = words.iter().map(|word| (keyword_label(word.trim()), word.trim().to_string())).collect();
        Ok((Self { spotter, stream, labels }, problems))
    }

    /// 喂入 16kHz 音频，检测到唤醒词时返回该词
    pub fn accept(&mut self, samples: &[f32]) -> Option<String> {
        self.stream.accept_waveform(16000, samples);
        while self.spotter.is_ready(&self.stream) {
            self.spotter.decode(&self.stream);
            if let Some(result) = self.spotter.get_result(&self.stream) {
                if !result.keyword.is_empty() {
                    self.spotter.reset(&self.stream);
                    return Some(self.labels.get(&result.keyword).cloned().unwrap_or(result.keyword));
                }
            }
        }
        None
    }

    /// 丢弃之前的状态（静音、识别结束后调用，避免残留音频误触发）
    pub fn reset(&mut self) {
        self.stream = self.spotter.create_stream();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_sherpa_ppinyin() {
        assert_eq!(word_tokens("周望军", &HashMap::new()).unwrap(), ["zh", "ōu", "w", "àng", "j", "ūn"]);
        assert_eq!(word_tokens("女儿", &HashMap::new()).unwrap(), ["n", "ǚ", "ér"]);
        assert_eq!(word_tokens("你好电脑", &HashMap::new()).unwrap(), ["n", "ǐ", "h", "ǎo", "d", "iàn", "n", "ǎo"]);
    }

    #[test]
    fn english_and_mixed_wake_words() {
        let lexicon = english_lexicon(include_str!("../models/kws/en.phone"));
        assert_eq!(word_tokens("Hey Aeri", &lexicon).unwrap(), ["HH", "EY1", "EH1", "R", "IY0"]);
        assert_eq!(word_tokens("hEy aErI", &lexicon).unwrap(), word_tokens("Hey Aeri", &lexicon).unwrap());
        assert_eq!(word_tokens("你好 Aeri", &lexicon).unwrap(), ["n", "ǐ", "h", "ǎo", "EH1", "R", "IY0"]);
        assert!(word_tokens("unlistedxyz", &lexicon).unwrap_err().contains("unlistedxyz"));
        assert!(word_tokens("Hey 123", &lexicon).is_err());
        let vocab = ["HH", "EY1", "EH1", "R", "IY0"].map(str::to_string).into_iter().collect();
        let (text, problems) = keywords_text(&["Hey Aeri".into(), "unknownxyz".into(), "你好".into()], &vocab, &lexicon);
        assert!(text.contains("HH EY1 EH1 R IY0 @Hey_Aeri"));
        assert_eq!(problems.len(), 2);
        assert_eq!(keyword_label("Hey   Aeri"), "Hey_Aeri");
    }

    #[test]
    fn language_defaults_preserve_saved_wake_words() {
        let fresh = crate::config::Config::default();
        assert_eq!(fresh.wake_words, [crate::locale::pick("Hey Aeri", "你好电脑")]);
        let saved: crate::config::Config = serde_json::from_str(r#"{"wake_words":["自定义唤醒词","Hello computer"]}"#).unwrap();
        assert_eq!(saved.wake_words, ["自定义唤醒词", "Hello computer"]);
    }
}
