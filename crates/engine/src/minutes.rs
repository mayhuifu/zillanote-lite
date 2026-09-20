//! Turns a transcript into minutes with whatever OpenAI-compatible endpoint the user has:
//! LM Studio or Ollama on this machine, or a hosted API with a key.

use std::time::Duration;

use crate::templates::Template;

/// Local models can take minutes on a long transcript.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(900);

#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// Transcripts longer than this are summarized in parts first.
    pub max_chars_per_call: usize,
}

#[derive(serde::Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(serde::Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(serde::Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(serde::Deserialize)]
struct ChoiceMessage {
    #[serde(default)]
    content: String,
}

/// `on_step(done, total)` reports each model call as it finishes.
pub async fn write_minutes(
    config: &LlmConfig,
    system_prompt: &str,
    template: &Template,
    title: &str,
    transcript_text: &str,
    on_step: impl Fn(usize, usize),
) -> Result<String, String> {
    if config.base_url.trim().is_empty() || config.model.trim().is_empty() {
        return Err("Choose a language model in Settings first.".to_string());
    }

    let parts = split_at_lines(transcript_text, config.max_chars_per_call.max(2_000));
    let total = if parts.len() > 1 { parts.len() + 1 } else { 1 };

    let source = if parts.len() == 1 {
        format!("Transcript:\n\n{}", parts[0])
    } else {
        // Too long for one call: take notes on each part, then write from the notes.
        let mut notes = Vec::with_capacity(parts.len());
        for (index, part) in parts.iter().enumerate() {
            let request = format!(
                "This is part {} of {} of a long transcript. Write detailed notes on this part only: \
                 the facts, figures, arguments, decisions, commitments and open questions, in order, \
                 keeping the [mm:ss] times. Do not write final minutes yet.\n\nTranscript part:\n\n{part}",
                index + 1,
                parts.len()
            );
            notes.push(chat(config, system_prompt, &request).await?);
            on_step(index + 1, total);
        }
        format!(
            "Notes taken on the consecutive parts of the transcript:\n\n{}",
            notes.join("\n\n---\n\n")
        )
    };

    let request = format!("{}\n\nMeeting title: {title}\n\n{source}", template.prompt);
    let minutes = chat(config, system_prompt, &request).await?;
    on_step(total, total);
    Ok(minutes)
}

async fn chat(config: &LlmConfig, system_prompt: &str, user: &str) -> Result<String, String> {
    let url = format!("{}/chat/completions", config.base_url.trim().trim_end_matches('/'));
    let mut client = reqwest::Client::builder().timeout(REQUEST_TIMEOUT);
    if is_loopback(&url) {
        // A system-wide proxy must not get between the app and a model on this machine.
        client = client.no_proxy();
    }
    let client = client.build().map_err(|e| e.to_string())?;

    let mut request = client.post(&url).json(&serde_json::json!({
        "model": config.model.trim(),
        "temperature": 0.2,
        "stream": false,
        "messages": [
            Message { role: "system", content: system_prompt },
            Message { role: "user", content: user },
        ],
    }));
    if !config.api_key.trim().is_empty() {
        request = request.bearer_auth(config.api_key.trim());
    }

    let response = request.send().await.map_err(|error| {
        if error.is_connect() {
            format!("Could not reach the language model at {url}. Is it running?")
        } else if error.is_timeout() {
            "The language model took too long to answer.".to_string()
        } else {
            format!("Language model request failed: {error}")
        }
    })?;

    let status = response.status();
    let body = response.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        let detail = body.chars().take(300).collect::<String>();
        return Err(format!("The language model answered {status}: {detail}"));
    }

    let parsed: ChatResponse = serde_json::from_str(&body)
        .map_err(|_| "The language model's answer was not in the expected format.".to_string())?;
    let content = parsed
        .choices
        .into_iter()
        .next()
        .map(|choice| strip_reasoning(&choice.message.content))
        .unwrap_or_default();
    if content.trim().is_empty() {
        return Err("The language model returned an empty answer.".to_string());
    }
    Ok(content)
}

pub(crate) fn is_loopback(url: &str) -> bool {
    ["://localhost", "://127.0.0.1", "://[::1]"]
        .iter()
        .any(|host| url.contains(host))
}

/// Reasoning models put their thinking in `<think>` blocks ahead of the answer.
fn strip_reasoning(content: &str) -> String {
    let mut text = content.to_string();
    while let Some(start) = text.find("<think>") {
        match text[start..].find("</think>") {
            Some(end) => text.replace_range(start..start + end + "</think>".len(), ""),
            None => text.truncate(start),
        }
    }
    text.trim().to_string()
}

/// Splits on line boundaries into pieces of at most `max_chars` characters. A single line
/// longer than that stays whole: cutting a sentence costs more than an oversized call.
fn split_at_lines(text: &str, max_chars: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut current_chars = 0;

    for line in text.lines() {
        let line_chars = line.chars().count() + 1;
        if current_chars > 0 && current_chars + line_chars > max_chars {
            parts.push(std::mem::take(&mut current));
            current_chars = 0;
        }
        current.push_str(line);
        current.push('\n');
        current_chars += line_chars;
    }
    if !current.trim().is_empty() || parts.is_empty() {
        parts.push(current);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_transcript_is_one_part_and_a_long_one_splits_between_lines() {
        assert_eq!(split_at_lines("one\ntwo", 100), vec!["one\ntwo\n"]);

        let text = (0..10).map(|i| format!("[00:0{i}] 这是第{i}句话。")).collect::<Vec<_>>();
        let parts = split_at_lines(&text.join("\n"), 40);

        assert!(parts.len() > 1);
        assert!(parts.iter().all(|part| part.chars().count() <= 40));
        assert_eq!(parts.concat().lines().count(), 10);
        assert!(parts.iter().all(|part| part.starts_with('[')));
    }

    #[test]
    fn thinking_is_removed_from_the_answer() {
        assert_eq!(
            strip_reasoning("<think>\nhmm\n</think>\n\n## Summary\nDone."),
            "## Summary\nDone."
        );
        assert_eq!(strip_reasoning("## Summary"), "## Summary");
        assert_eq!(strip_reasoning("Answer<think>cut off"), "Answer");
    }

    #[test]
    fn only_this_machine_counts_as_loopback() {
        assert!(is_loopback("http://localhost:1234/v1/chat/completions"));
        assert!(is_loopback("http://127.0.0.1:8080/v1"));
        assert!(!is_loopback("https://api.openai.com/v1"));
    }

    #[tokio::test]
    async fn minutes_need_a_configured_model() {
        let config = LlmConfig {
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            max_chars_per_call: 24_000,
        };

        let error = write_minutes(
            &config,
            "system",
            crate::templates::template("discussion"),
            "Title",
            "[00:00] hello",
            |_, _| {},
        )
        .await
        .unwrap_err();

        assert!(error.contains("Settings"));
    }
}
