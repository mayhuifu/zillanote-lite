const ASR_TEXT_TAG: &str = "<asr_text>";
const LANGUAGE_MARKER: &str = "language ";
const MAX_UNPUNCTUATED_CJK_CHARS: usize = 16;
const MIN_WORD_SECONDS: f64 = 0.05;

/// One display unit of a transcript with the time span it was spoken in.
#[derive(Debug, Clone, PartialEq)]
pub struct TimedWord {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

/// llama.cpp leaks the model's `language English<asr_text>` preamble to clients, and a
/// code-switched chunk can carry more than one.
pub fn strip_asr_prefix(raw: &str) -> String {
    let mut cleaned = String::with_capacity(raw.len());
    let mut rest = raw;

    while let Some(tag_at) = rest.find(ASR_TEXT_TAG) {
        let before = &rest[..tag_at];
        let kept = match before.rfind(LANGUAGE_MARKER) {
            Some(marker_at) if is_language_name(&before[marker_at + LANGUAGE_MARKER.len()..]) => {
                &before[..marker_at]
            }
            _ => before,
        };
        cleaned.push_str(kept);
        if !kept.is_empty() && !kept.ends_with(char::is_whitespace) {
            cleaned.push(' ');
        }
        rest = &rest[tag_at + ASR_TEXT_TAG.len()..];
    }
    cleaned.push_str(rest);

    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_language_name(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_alphabetic())
}

/// Qwen3-ASR returns no timestamps, so words share their chunk's real span in proportion
/// to how long they take to say. Chinese has no spaces: it is cut into clauses at
/// punctuation so speaker labels can change inside a chunk without the transcript view
/// (which joins words with spaces) breaking up phrases.
pub fn timed_words(text: &str, start: f64, duration: f64) -> Vec<TimedWord> {
    let units = split_units(text);
    if units.is_empty() {
        return Vec::new();
    }

    let duration = duration.max(MIN_WORD_SECONDS * units.len() as f64);
    let total_weight: f64 = units.iter().map(|unit| spoken_weight(unit)).sum();
    let mut elapsed = 0.0;

    units
        .into_iter()
        .map(|unit| {
            let word_start = start + duration * elapsed / total_weight;
            elapsed += spoken_weight(&unit);
            let word_end = start + duration * elapsed / total_weight;
            TimedWord {
                text: unit,
                start: word_start,
                end: word_end,
            }
        })
        .collect()
}

fn split_units(text: &str) -> Vec<String> {
    text.split_whitespace()
        .flat_map(|token| {
            if token.chars().any(is_unspaced_script) {
                split_clauses(token)
            } else {
                vec![token.to_string()]
            }
        })
        .collect()
}

fn split_clauses(token: &str) -> Vec<String> {
    let mut clauses = Vec::new();
    let mut current = String::new();
    let mut current_cjk_chars = 0usize;
    let mut chars = token.chars().peekable();

    while let Some(c) = chars.next() {
        current.push(c);
        if is_unspaced_script(c) {
            current_cjk_chars += 1;
        }

        let next_is_punctuation = chars.peek().copied().is_some_and(is_clause_punctuation);
        let clause_ended = is_clause_punctuation(c) && !next_is_punctuation;
        let too_long = current_cjk_chars >= MAX_UNPUNCTUATED_CJK_CHARS && !next_is_punctuation;
        if clause_ended || too_long {
            clauses.push(std::mem::take(&mut current));
            current_cjk_chars = 0;
        }
    }
    if !current.is_empty() {
        clauses.push(current);
    }
    clauses
}

/// A Chinese character takes about as long to say as three Latin letters.
fn spoken_weight(unit: &str) -> f64 {
    let cjk = unit.chars().filter(|c| is_unspaced_script(*c)).count() as f64;
    let other = unit
        .chars()
        .filter(|c| c.is_alphanumeric() && !is_unspaced_script(*c))
        .count() as f64;
    (cjk + other / 3.0).max(1.0)
}

fn is_unspaced_script(c: char) -> bool {
    matches!(u32::from(c),
        0x3040..=0x30FF      // Hiragana, Katakana
        | 0x3400..=0x4DBF    // CJK extension A
        | 0x4E00..=0x9FFF    // CJK unified ideographs
        | 0xF900..=0xFAFF    // CJK compatibility ideographs
        | 0x20000..=0x2A6DF  // CJK extension B
    )
}

fn is_clause_punctuation(c: char) -> bool {
    matches!(c, '，' | '。' | '！' | '？' | '；' | '：' | '、' | '…')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(words: &[TimedWord]) -> Vec<&str> {
        words.iter().map(|word| word.text.as_str()).collect()
    }

    #[test]
    fn strips_the_leaked_language_preamble() {
        assert_eq!(
            strip_asr_prefix("language Chinese<asr_text>在我视频看这个节目。"),
            "在我视频看这个节目。"
        );
        assert_eq!(
            strip_asr_prefix("language English<asr_text> Hello there. "),
            "Hello there."
        );
    }

    #[test]
    fn strips_every_preamble_in_a_code_switched_chunk() {
        assert_eq!(
            strip_asr_prefix(
                "language English<asr_text>Let's start.language Chinese<asr_text>我们开始吧。"
            ),
            "Let's start. 我们开始吧。"
        );
    }

    #[test]
    fn keeps_text_that_only_looks_like_a_preamble() {
        assert_eq!(
            strip_asr_prefix("We should pick a language first."),
            "We should pick a language first."
        );
        assert_eq!(strip_asr_prefix("plain text"), "plain text");
        assert_eq!(strip_asr_prefix("<asr_text>tag only"), "tag only");
    }

    #[test]
    fn english_words_split_on_spaces_and_fill_the_chunk() {
        let words = timed_words("Ship the release today", 10.0, 2.0);

        assert_eq!(texts(&words), ["Ship", "the", "release", "today"]);
        assert_eq!(words.first().unwrap().start, 10.0);
        assert!((words.last().unwrap().end - 12.0).abs() < 1e-9);
        for pair in words.windows(2) {
            assert!((pair[0].end - pair[1].start).abs() < 1e-9);
        }
    }

    #[test]
    fn chinese_is_cut_into_clauses_at_punctuation() {
        let words = timed_words("我猜你最关心两个问题：这阵风能不能过去？好的。", 0.0, 6.0);

        assert_eq!(
            texts(&words),
            ["我猜你最关心两个问题：", "这阵风能不能过去？", "好的。"]
        );
    }

    #[test]
    fn longer_clauses_get_more_of_the_chunk() {
        let words = timed_words("这阵风能不能过去？好的。", 0.0, 10.0);

        let long = words[0].end - words[0].start;
        let short = words[1].end - words[1].start;
        assert!(long > short * 3.0, "{long} vs {short}");
    }

    #[test]
    fn mixed_language_keeps_english_words_and_chinese_clauses() {
        let words = timed_words("我们用 llama server 来跑，效果不错。", 0.0, 4.0);

        assert_eq!(
            texts(&words),
            ["我们用", "llama", "server", "来跑，", "效果不错。"]
        );
    }

    #[test]
    fn unpunctuated_chinese_is_still_bounded() {
        let text = "一".repeat(40);
        let words = timed_words(&text, 0.0, 8.0);

        assert_eq!(words.len(), 3);
        assert!(words.iter().all(|word| word.text.chars().count() <= 16));
    }

    #[test]
    fn repeated_punctuation_stays_with_its_clause() {
        let words = timed_words("真的吗？！好。", 0.0, 2.0);

        assert_eq!(texts(&words), ["真的吗？！", "好。"]);
    }

    #[test]
    fn empty_text_has_no_words() {
        assert!(timed_words("   ", 0.0, 1.0).is_empty());
    }
}
