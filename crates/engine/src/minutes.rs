//! Turns a transcript into minutes with whatever OpenAI-compatible endpoint the user has:
//! LM Studio or Ollama on this machine, or a hosted API with a key. Or on the user's ChatGPT
//! plan, after Sign in with ChatGPT, through the Responses API.

use std::time::Duration;

use crate::templates::Template;

/// Local models can take minutes on a long transcript.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(900);

/// How the model is asked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LlmApi {
    /// `/chat/completions`, which every OpenAI-compatible server has.
    #[default]
    ChatCompletions,
    /// `/responses` on the user's ChatGPT plan: the answer streamed, nothing stored, and the
    /// system prompt as `instructions`, because a system message is turned away there. The
    /// key is the sign-in's access token.
    ChatGptPlan,
}

#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub api: LlmApi,
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

/// What the model is told about the meeting besides what was said in it.
#[derive(Debug, Clone, Copy, Default)]
pub struct About<'a> {
    pub title: &'a str,
    /// Like "2026-09-20 (Sunday)", so that "tomorrow" and "Tuesday" can become dates.
    pub date: &'a str,
    /// The user's list of names and terms, in their correct spelling.
    pub known_terms: &'a [String],
}

impl About<'_> {
    fn lines(&self) -> String {
        let mut lines = vec![format!("Meeting title: {}", self.title)];
        if !self.date.is_empty() {
            lines.push(format!("Meeting date: {}", self.date));
        }
        if !self.known_terms.is_empty() {
            lines.push(format!("Known names and terms, correctly spelled: {}", self.known_terms.join(", ")));
        }
        lines.join("\n")
    }
}

/// `on_step(done, total)` reports each model call as it finishes.
pub async fn write_minutes(
    config: &LlmConfig,
    system_prompt: &str,
    template: &Template,
    about: About<'_>,
    transcript_text: &str,
    on_step: impl Fn(usize, usize),
) -> Result<String, String> {
    if config.base_url.trim().is_empty() || config.model.trim().is_empty() {
        return Err("Choose a language model in Settings first.".to_string());
    }
    let system_prompt = &crate::templates::effective_system_prompt(system_prompt);

    let parts = split_at_lines(transcript_text, config.max_chars_per_call.max(2_000));
    let total = if parts.len() > 1 { parts.len() + 1 } else { 1 };

    let source = if parts.len() == 1 {
        format!("Transcript:\n\n{}", parts[0])
    } else {
        // Too long for one call: take notes on each part, then write from the notes.
        let mut notes = Vec::with_capacity(parts.len());
        for (index, part) in parts.iter().enumerate() {
            notes.push(chat(config, system_prompt, &notes_request(template, about, index, parts.len(), part)).await?);
            on_step(index + 1, total);
        }
        format!(
            "Notes taken on the consecutive parts of the transcript:\n\n{}",
            notes.join("\n\n---\n\n")
        )
    };

    let request = format!("{}\n\n{}\n\n{source}", template.prompt, about.lines());
    let minutes = chat(config, system_prompt, &request).await?;
    on_step(total, total);
    Ok(minutes)
}

/// Notes are material for the minutes, not minutes: by topic, with everything a decision
/// hangs on, and with times only when the template will want them.
fn notes_request(template: &Template, about: About<'_>, index: usize, count: usize, part: &str) -> String {
    let times = if template.wants_times {
        " Note the [mm:ss] time at which each topic starts."
    } else {
        " Leave the [mm:ss] times out."
    };
    format!(
        "This is part {} of {count} of a long transcript. Do not write minutes yet. Write notes on this part only, \
         grouped by topic: the conclusions and positions with their reasons, the figures, the decisions, the \
         commitments with who made them, and what was left open. Correct mis-heard terms as your instructions \
         say.{times}\n\n{}\n\nTranscript part:\n\n{part}",
        index + 1,
        about.lines()
    )
}

/// Pauses before the second and third try of a call that failed for a passing reason.
const RETRY_PAUSES: [Duration; 2] = [Duration::from_secs(5), Duration::from_secs(20)];

/// A call to the model, tried again when it failed for a reason that passes by itself: no
/// connection, no answer in time, a busy or overloaded server. The minutes are written with
/// nobody watching, often right after an hour of other work, and one hiccup should not
/// leave a meeting without them. A wrong key or an unknown model is not tried again.
async fn chat(config: &LlmConfig, system_prompt: &str, user: &str) -> Result<String, String> {
    let mut pauses = RETRY_PAUSES.iter();
    loop {
        match chat_once(config, system_prompt, user).await {
            Err(failure) if failure.passing => match pauses.next() {
                Some(pause) => {
                    tracing::warn!(error = %failure.message, "minutes_call_retried");
                    tokio::time::sleep(scaled(*pause)).await;
                }
                None => return Err(failure.message),
            },
            result => return result.map_err(|failure| failure.message),
        }
    }
}

/// Tests do not wait out the real pauses.
fn scaled(pause: Duration) -> Duration {
    if cfg!(test) { pause / 1_000 } else { pause }
}

struct Failure {
    message: String,
    /// Worth another try a little later.
    passing: bool,
}

impl Failure {
    fn lasting(message: String) -> Self {
        Self { message, passing: false }
    }
}

async fn chat_once(config: &LlmConfig, system_prompt: &str, user: &str) -> Result<String, Failure> {
    let base = config.base_url.trim().trim_end_matches('/');
    let model = config.model.trim();
    let (url, body) = match config.api {
        LlmApi::ChatCompletions => (
            format!("{base}/chat/completions"),
            serde_json::json!({
                "model": model,
                "temperature": 0.2,
                "stream": false,
                "messages": [
                    Message { role: "system", content: system_prompt },
                    Message { role: "user", content: user },
                ],
            }),
        ),
        // No temperature: the plan does not take one.
        LlmApi::ChatGptPlan => (
            format!("{base}/responses"),
            serde_json::json!({
                "model": model,
                "instructions": system_prompt,
                "input": [Message { role: "user", content: user }],
                "store": false,
                "stream": true,
            }),
        ),
    };
    let mut client = reqwest::Client::builder().timeout(REQUEST_TIMEOUT);
    if is_loopback(&url) {
        // A system-wide proxy must not get between the app and a model on this machine.
        client = client.no_proxy();
    }
    let client = client.build().map_err(|e| Failure::lasting(e.to_string()))?;

    let mut request = client.post(&url).json(&body);
    if !config.api_key.trim().is_empty() {
        request = request.bearer_auth(config.api_key.trim());
    }

    let response = request.send().await.map_err(|error| Failure {
        message: if error.is_connect() {
            format!("Could not reach the language model at {url}. Is it running?")
        } else if error.is_timeout() {
            "The language model took too long to answer.".to_string()
        } else {
            format!("Language model request failed: {error}")
        },
        passing: true,
    })?;

    let status = response.status();
    if !status.is_success() || config.api == LlmApi::ChatCompletions {
        // The connection dropping while the answer comes in passes too.
        let body = response.text().await.map_err(broke_off)?;
        if !status.is_success() {
            if let Some((message, passing)) = crate::chatgpt::error_code(&body)
                .filter(|_| config.api == LlmApi::ChatGptPlan)
                .and_then(|code| crate::chatgpt::plan_failure(&code))
            {
                return Err(Failure { message, passing });
            }
            let detail = body.chars().take(300).collect::<String>();
            return Err(Failure {
                message: format!("The language model answered {status}: {detail}"),
                // Too many requests, or the server's own trouble. Anything else 4xx is ours.
                passing: status.as_u16() == 429 || status.is_server_error(),
            });
        }
        let parsed: ChatResponse = serde_json::from_str(&body)
            .map_err(|_| Failure::lasting("The language model's answer was not in the expected format.".to_string()))?;
        return finished(parsed.choices.into_iter().next().map(|choice| choice.message.content).unwrap_or_default());
    }
    finished(streamed_answer(response).await?)
}

fn broke_off(error: reqwest::Error) -> Failure {
    Failure {
        message: format!("The language model's answer broke off: {error}"),
        passing: true,
    }
}

fn finished(answer: String) -> Result<String, Failure> {
    let content = strip_reasoning(&answer);
    if content.trim().is_empty() {
        return Err(Failure::lasting("The language model returned an empty answer.".to_string()));
    }
    Ok(content)
}

/// Reads a streamed Responses answer: text arrives in `response.output_text.delta` events, and
/// only `response.completed` says it is whole.
async fn streamed_answer(mut response: reqwest::Response) -> Result<String, Failure> {
    let mut pending: Vec<u8> = Vec::new();
    let mut text = String::new();
    loop {
        let Some(chunk) = response.chunk().await.map_err(broke_off)? else {
            return Err(Failure {
                message: "The language model's answer broke off before it was complete.".to_string(),
                passing: true,
            });
        };
        pending.extend_from_slice(&chunk);
        // Whole lines only: a chunk can end inside a line, or inside a character.
        while let Some(end) = pending.iter().position(|&byte| byte == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let Some(data) = line.trim().strip_prefix("data:") else {
                continue;
            };
            let Ok(event) = serde_json::from_str::<serde_json::Value>(data.trim()) else {
                continue;
            };
            match event["type"].as_str().unwrap_or_default() {
                "response.output_text.delta" => text.push_str(event["delta"].as_str().unwrap_or_default()),
                "response.completed" => {
                    if text.is_empty() {
                        text = output_text(&event["response"]);
                    }
                    return Ok(text);
                }
                "response.failed" => return Err(failed(&event["response"]["error"])),
                "error" => return Err(failed(&event)),
                "response.incomplete" => {
                    let reason = event["response"]["incomplete_details"]["reason"].as_str().unwrap_or("no reason given");
                    return Err(Failure::lasting(format!("The language model stopped before the minutes were complete ({reason}).")));
                }
                _ => {}
            }
        }
    }
}

/// The text of a finished response, for an answer that came without deltas.
fn output_text(response: &serde_json::Value) -> String {
    let items = response["output"].as_array().into_iter().flatten();
    items
        .flat_map(|item| item["content"].as_array().into_iter().flatten())
        .filter(|part| part["type"] == "output_text")
        .filter_map(|part| part["text"].as_str())
        .collect()
}

fn failed(error: &serde_json::Value) -> Failure {
    let code = error["code"].as_str().unwrap_or_default();
    if let Some((message, passing)) = crate::chatgpt::plan_failure(code) {
        return Failure { message, passing };
    }
    let message = error["message"].as_str().filter(|message| !message.is_empty()).unwrap_or(code);
    Failure {
        message: format!("The language model failed: {message}"),
        passing: matches!(code, "server_error" | "rate_limit_exceeded"),
    }
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

    fn about() -> About<'static> {
        About {
            title: "Title",
            ..About::default()
        }
    }

    fn config_for(server: &wiremock::MockServer) -> LlmConfig {
        LlmConfig {
            api: LlmApi::ChatCompletions,
            base_url: server.uri(),
            api_key: "key".to_string(),
            model: "model".to_string(),
            max_chars_per_call: 24_000,
        }
    }

    fn answer(text: &str) -> wiremock::ResponseTemplate {
        wiremock::ResponseTemplate::new(200)
            .set_body_json(serde_json::json!({ "choices": [{ "message": { "content": text } }] }))
    }

    #[tokio::test]
    async fn a_busy_server_is_asked_again_and_the_minutes_still_arrive() {
        use wiremock::matchers::method;
        let server = wiremock::MockServer::start().await;
        // Busy twice, then well: the mocks are tried in the order they were mounted.
        wiremock::Mock::given(method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(503).set_body_string("overloaded"))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        wiremock::Mock::given(method("POST")).respond_with(answer("## Summary\nDone.")).mount(&server).await;

        let template = crate::templates::template("discussion");
        let minutes = write_minutes(&config_for(&server), "system", template, about(), "[00:00] hello", |_, _| {}).await;

        assert_eq!(minutes.unwrap(), "## Summary\nDone.");
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn the_model_is_given_the_date_the_known_terms_and_the_shape_of_the_minutes() {
        use wiremock::matchers::method;
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(method("POST")).respond_with(answer("## 1. Topic")).mount(&server).await;
        let terms = ["U300".to_string(), "RedCap".to_string()];
        let about = About {
            title: "SP review",
            date: "2026-09-20 (Sunday)",
            known_terms: &terms,
        };

        // The user's own rules name no shape, so ours goes along.
        let template = crate::templates::template("business");
        write_minutes(&config_for(&server), "Be brief.", template, about, "[00:00] hello", |_, _| {}).await.unwrap();

        let sent: serde_json::Value = server.received_requests().await.unwrap()[0].body_json().unwrap();
        let (system, user) = (sent["messages"][0]["content"].as_str().unwrap(), sent["messages"][1]["content"].as_str().unwrap());
        assert!(system.starts_with("Be brief.") && system.contains("## AI suggestions"), "{system}");
        assert!(user.contains("Meeting date: 2026-09-20 (Sunday)"), "{user}");
        assert!(user.contains("Known names and terms, correctly spelled: U300, RedCap"), "{user}");
    }

    #[test]
    fn notes_on_a_long_transcript_keep_times_only_for_the_template_that_shows_them() {
        let about = about();
        let flow = notes_request(crate::templates::template("discussion"), about, 0, 2, "[00:00] hi");
        let business = notes_request(crate::templates::template("business"), about, 0, 2, "[00:00] hi");

        assert!(flow.contains("time at which each topic starts"));
        assert!(business.contains("Leave the [mm:ss] times out"));
        assert!(business.contains("part 1 of 2") && business.contains("Do not write minutes yet"));
    }

    #[tokio::test]
    async fn a_wrong_key_is_reported_at_once_and_not_tried_again() {
        use wiremock::matchers::method;
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(401).set_body_string("invalid api key"))
            .mount(&server)
            .await;

        let template = crate::templates::template("discussion");
        let error = write_minutes(&config_for(&server), "system", template, about(), "[00:00] hello", |_, _| {})
            .await
            .unwrap_err();

        assert!(error.contains("401") && error.contains("invalid api key"), "{error}");
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    fn plan_config(server: &wiremock::MockServer) -> LlmConfig {
        LlmConfig {
            api: LlmApi::ChatGptPlan,
            ..config_for(server)
        }
    }

    /// A Responses stream as the server sends it: one event per `data:` line.
    fn stream(events: &[serde_json::Value]) -> wiremock::ResponseTemplate {
        let body: String = events
            .iter()
            .map(|event| format!("event: {}\ndata: {event}\n\n", event["type"].as_str().unwrap()))
            .collect();
        wiremock::ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_string(body)
    }

    #[tokio::test]
    async fn on_the_chatgpt_plan_the_answer_is_streamed_and_nothing_is_stored() {
        use wiremock::matchers::{method, path};
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(stream(&[
                serde_json::json!({ "type": "response.created" }),
                serde_json::json!({ "type": "response.output_text.delta", "delta": "## Sum" }),
                serde_json::json!({ "type": "response.output_text.delta", "delta": "mary 总结\nDone." }),
                serde_json::json!({ "type": "response.completed", "response": {} }),
            ]))
            .mount(&server)
            .await;

        let template = crate::templates::template("discussion");
        let minutes = write_minutes(&plan_config(&server), "Be brief.", template, about(), "[00:00] hello", |_, _| {}).await;

        assert_eq!(minutes.unwrap(), "## Summary 总结\nDone.");
        let sent: serde_json::Value = server.received_requests().await.unwrap()[0].body_json().unwrap();
        assert_eq!((sent["store"].as_bool(), sent["stream"].as_bool()), (Some(false), Some(true)));
        assert!(sent["instructions"].as_str().unwrap().starts_with("Be brief."));
        assert_eq!(sent["input"][0]["role"], "user");
        // The plan turns these away.
        assert!(sent.get("temperature").is_none() && sent.get("messages").is_none(), "{sent}");
    }

    #[tokio::test]
    async fn an_answer_without_deltas_is_taken_from_the_finished_response() {
        use wiremock::matchers::method;
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(method("POST"))
            .respond_with(stream(&[serde_json::json!({ "type": "response.completed", "response": { "output": [
                { "type": "reasoning", "content": [] },
                { "type": "message", "content": [{ "type": "output_text", "text": "## Summary" }] },
            ]}})]))
            .mount(&server)
            .await;

        let template = crate::templates::template("discussion");
        let minutes = write_minutes(&plan_config(&server), "s", template, about(), "[00:00] hello", |_, _| {}).await;

        assert_eq!(minutes.unwrap(), "## Summary");
    }

    #[tokio::test]
    async fn a_used_up_plan_says_so_at_once_whether_it_is_told_in_the_stream_or_by_the_status() {
        use wiremock::matchers::method;
        for refusal in [
            stream(&[serde_json::json!({ "type": "response.failed", "response": { "error": {
                "code": "subscription_sharing_usage_limit_exceeded", "message": "limit" } } })]),
            wiremock::ResponseTemplate::new(429).set_body_json(serde_json::json!({ "error": {
                "code": "subscription_sharing_usage_limit_exceeded", "message": "limit" } })),
        ] {
            let server = wiremock::MockServer::start().await;
            wiremock::Mock::given(method("POST")).respond_with(refusal).mount(&server).await;

            let template = crate::templates::template("discussion");
            let error = write_minutes(&plan_config(&server), "s", template, about(), "[00:00] hello", |_, _| {})
                .await
                .unwrap_err();

            assert!(error.starts_with(crate::chatgpt::USAGE_LIMIT), "{error}");
            // Waiting a minute does not bring the plan's allowance back.
            assert_eq!(server.received_requests().await.unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn a_stream_that_ends_before_the_response_is_complete_is_tried_again() {
        use wiremock::matchers::method;
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(method("POST"))
            .respond_with(stream(&[serde_json::json!({ "type": "response.output_text.delta", "delta": "## Half" })]))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        wiremock::Mock::given(method("POST"))
            .respond_with(stream(&[
                serde_json::json!({ "type": "response.output_text.delta", "delta": "## Whole" }),
                serde_json::json!({ "type": "response.completed", "response": {} }),
            ]))
            .mount(&server)
            .await;

        let template = crate::templates::template("discussion");
        let minutes = write_minutes(&plan_config(&server), "s", template, about(), "[00:00] hello", |_, _| {}).await;

        assert_eq!(minutes.unwrap(), "## Whole");
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn minutes_need_a_configured_model() {
        let config = LlmConfig {
            api: LlmApi::ChatCompletions,
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            max_chars_per_call: 24_000,
        };

        let error = write_minutes(
            &config,
            "system",
            crate::templates::template("discussion"),
            about(),
            "[00:00] hello",
            |_, _| {},
        )
        .await
        .unwrap_err();

        assert!(error.contains("Settings"));
    }
}
