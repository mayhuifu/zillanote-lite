//! Sends the minutes to one fixed address through the user's own mail account.

use std::time::Duration;

use lettre::message::{Mailbox, MultiPart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct EmailSettings {
    /// Where every set of minutes goes.
    pub to: String,
    /// The account that sends them.
    pub from: String,
    /// That account's app password (Gmail, iCloud, Yahoo) or authorization code (QQ, 163).
    pub password: String,
    /// `host` or `host:port`. Empty for the providers the app already knows.
    pub server: String,
}

impl EmailSettings {
    /// Minutes are emailed whenever these three are filled in.
    pub fn is_configured(&self) -> bool {
        [&self.to, &self.from, &self.password]
            .iter()
            .all(|field| !field.trim().is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub host: String,
    pub port: u16,
}

const KNOWN_SERVERS: &[(&[&str], &str, u16)] = &[
    (&["gmail.com", "googlemail.com"], "smtp.gmail.com", 465),
    (&["qq.com", "foxmail.com"], "smtp.qq.com", 465),
    (&["163.com"], "smtp.163.com", 465),
    (&["126.com"], "smtp.126.com", 465),
    (&["outlook.com", "hotmail.com", "live.com", "msn.com"], "smtp-mail.outlook.com", 587),
    (&["icloud.com", "me.com", "mac.com"], "smtp.mail.me.com", 587),
    (&["yahoo.com"], "smtp.mail.yahoo.com", 465),
];

pub fn server_for(settings: &EmailSettings) -> Result<Server, String> {
    let custom = settings.server.trim();
    if !custom.is_empty() {
        let (host, port) = match custom.rsplit_once(':') {
            Some((host, port)) => (
                host,
                port.parse().map_err(|_| format!("\"{port}\" is not a port number."))?,
            ),
            None => (custom, 465),
        };
        return Ok(Server { host: host.to_string(), port });
    }

    let domain = settings.from.trim().rsplit('@').next().unwrap_or_default().to_lowercase();
    KNOWN_SERVERS
        .iter()
        .find(|(domains, _, _)| domains.contains(&domain.as_str()))
        .map(|(_, host, port)| Server { host: host.to_string(), port: *port })
        .ok_or_else(|| format!("Enter the outgoing mail (SMTP) server for {domain} in Settings."))
}

pub fn minutes_message(
    settings: &EmailSettings,
    title: &str,
    when: &str,
    minutes_markdown: &str,
) -> Result<Message, String> {
    let from: Mailbox = format!("ZillaNote <{}>", settings.from.trim())
        .parse()
        .map_err(|_| format!("\"{}\" is not an email address.", settings.from.trim()))?;
    let to: Mailbox = settings
        .to
        .trim()
        .parse()
        .map_err(|_| format!("\"{}\" is not an email address.", settings.to.trim()))?;

    let plain = format!("{title}\n{when}\n\n{minutes_markdown}\n\n-- \nSent by ZillaNote from this computer.");
    let html = format!(
        "<div style=\"font:15px/1.55 -apple-system,'Segoe UI','PingFang SC','Microsoft YaHei',sans-serif;color:#1f1d1a;max-width:720px\">\
         <h1 style=\"font-size:20px;margin:0 0 2px\">{}</h1><p style=\"color:#77726b;margin:0 0 18px;font-size:13px\">{}</p>{}\
         <p style=\"color:#9a958d;font-size:12px;margin-top:28px\">Sent by ZillaNote from this computer.</p></div>",
        escape(title),
        escape(when),
        render_html(minutes_markdown)
    );

    Message::builder()
        .from(from)
        .to(to)
        .subject(subject(title, when, minutes_markdown))
        .multipart(MultiPart::alternative_plain_html(plain, html))
        .map_err(|e| e.to_string())
}

/// "Minutes 2026-09-21: Budget and revenue". The day is enough in a subject line; what tells
/// one meeting's mail from another's is what the meeting was about.
pub fn subject(title: &str, when: &str, minutes_markdown: &str) -> String {
    let day = when.get(..10).filter(|day| chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").is_ok());
    let topic = main_topic(title, minutes_markdown);
    match day {
        Some(day) => format!("Minutes {day}: {topic}"),
        None => format!("Minutes: {topic}"),
    }
}

/// The name the user gave the meeting; else the first heading of the minutes that says
/// something. Models like to open with the title they were handed ("Meeting 2026-09-20
/// 16:34") or a section name ("Overview"), and those say nothing.
fn main_topic(title: &str, minutes_markdown: &str) -> String {
    let headings = minutes_markdown.lines().filter_map(|line| {
        let line = line.trim_start();
        let text = line.strip_prefix("# ").or_else(|| line.strip_prefix("## "))?;
        // "## 1. Budget" and "## 一、预算": the number is the template's, not the topic's.
        let text = text.trim().trim_start_matches(|c: char| c.is_ascii_digit() || "一二三四五六七八九十".contains(c));
        let text = text.trim_start_matches(['.', '、', ')', '）', ':', '：', ' ']);
        Some(text.replace(['*', '_', '`'], "").trim().to_string())
    });
    std::iter::once(title.trim().to_string())
        .chain(headings)
        .find(|candidate| says_something(candidate))
        .unwrap_or_else(|| "Meeting".to_string())
}

fn says_something(text: &str) -> bool {
    const EMPTY_WORDS: &[&str] = &[
        "meeting", "minutes", "summary", "overview", "discussion", "key", "points", "notes", "of", "the",
        "会议", "纪要", "记录", "总结", "概述", "概要", "讨论", "要点", "年", "月", "日",
    ];
    let mut rest = text.to_lowercase();
    for word in EMPTY_WORDS {
        rest = rest.replace(word, "");
    }
    rest.chars().filter(|c| c.is_alphabetic()).count() >= 2
}

/// Markdown to HTML for the mail body. Minutes come from a language model, so any raw HTML
/// in them is shown as text rather than passed through.
pub fn render_html(markdown: &str) -> String {
    use pulldown_cmark::{Event, Options, Parser, html};

    let parser = Parser::new_ext(markdown, Options::ENABLE_TABLES).map(|event| match event {
        Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
        other => other,
    });
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out.replace("<table>", "<table style=\"border-collapse:collapse\" border=\"1\" cellpadding=\"6\">")
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub async fn send(settings: &EmailSettings, message: Message) -> Result<(), String> {
    let server = server_for(settings)?;
    // 465 is TLS from the first byte; anything else starts plain and upgrades.
    let builder = if server.port == 465 {
        AsyncSmtpTransport::<Tokio1Executor>::relay(&server.host)
    } else {
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&server.host)
    }
    .map_err(|e| e.to_string())?;

    let transport = builder
        .port(server.port)
        .credentials(Credentials::new(settings.from.trim().to_string(), password(settings)))
        .timeout(Some(Duration::from_secs(30)))
        .build();

    transport.send(message).await.map(|_| ()).map_err(|error| explain(&server, &error.to_string()))
}

/// A refused login gets advice; anything else is passed on as it came. Servers refuse a
/// login with 535, and Gmail with 534 when it wants an app password.
fn explain(server: &Server, detail: &str) -> String {
    let lower = detail.to_lowercase();
    let refused = ["(534)", "(535)"].iter().any(|code| detail.contains(code))
        || ["authentication", "password", "credentials"].iter().any(|word| lower.contains(word));
    if refused {
        format!("{} did not accept the address or password. {} ({detail})", server.host, advice(&server.host))
    } else {
        format!("Could not send through {}:{}: {detail}", server.host, server.port)
    }
}

/// Google shows an app password as four groups of four letters, and that is how it gets
/// pasted. The sixteen letters are the password.
fn password(settings: &EmailSettings) -> String {
    let typed = settings.password.trim();
    let groups = typed.split_whitespace().collect::<Vec<_>>();
    let grouped = groups.len() == 4
        && groups.iter().all(|group| group.len() == 4 && group.chars().all(|c| c.is_ascii_alphabetic()));
    if grouped { groups.concat() } else { typed.to_string() }
}

/// What to do about a refused password, which differs by provider.
fn advice(host: &str) -> &'static str {
    match host {
        "smtp.gmail.com" => {
            "Gmail never takes the Google password here, only an app password: turn on 2-Step \
             Verification, then make one at https://myaccount.google.com/apppasswords."
        }
        "smtp.qq.com" | "smtp.163.com" | "smtp.126.com" => {
            "It takes an authorization code, not the login password: switch on SMTP in the mailbox's \
             settings on the web, which hands out the code."
        }
        _ => "Most providers take an app password here, not the login password.",
    }
}

pub async fn send_test(settings: &EmailSettings) -> Result<(), String> {
    let message = minutes_message(
        settings,
        "ZillaNote test",
        "If you can read this, minutes will arrive here.",
        "## It works\n\nMinutes are sent to this address as soon as they are written.",
    )?;
    send(settings, message).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(from: &str, server: &str) -> EmailSettings {
        EmailSettings {
            to: "hui@example.com".to_string(),
            from: from.to_string(),
            password: "app-password".to_string(),
            server: server.to_string(),
        }
    }

    #[test]
    fn the_server_comes_from_the_senders_domain_unless_one_is_given() {
        let server = |from, custom| server_for(&settings(from, custom));

        assert_eq!(server("me@Gmail.com", "").unwrap(), Server { host: "smtp.gmail.com".into(), port: 465 });
        assert_eq!(server("me@qq.com", "").unwrap().host, "smtp.qq.com");
        assert_eq!(server("me@outlook.com", "").unwrap().port, 587);
        assert_eq!(server("me@corp.example", "mail.corp.example:587").unwrap(), Server { host: "mail.corp.example".into(), port: 587 });
        assert_eq!(server("me@corp.example", "mail.corp.example").unwrap().port, 465);
        assert!(server("me@corp.example", "").unwrap_err().contains("corp.example"));
        assert!(server("me@corp.example", "host:abc").is_err());
    }

    #[test]
    fn a_refused_login_comes_with_advice_and_other_failures_as_they_are() {
        let gmail = Server { host: "smtp.gmail.com".into(), port: 465 };
        // What Gmail answers to the Google password of an account with 2-Step Verification.
        let wants_app_password = "permanent error (534): 5.7.9 Application-specific password required. \
                                  For more information, go to https://support.google.com/mail/?p=InvalidSecondFactor";

        let refused = explain(&gmail, wants_app_password);
        assert!(refused.contains("did not accept") && refused.contains("myaccount.google.com/apppasswords"), "{refused}");
        assert!(refused.contains("(534)"), "the server's own words stay in: {refused}");
        assert!(explain(&gmail, "permanent error (535): 5.7.8 Username and Password not accepted").contains("app password"));

        let untrusted = explain(&gmail, "Connection error: invalid peer certificate: unknown certificate authority");
        assert!(untrusted.starts_with("Could not send through"), "not a matter of passwords: {untrusted}");

        let unreachable = explain(&gmail, "Connection error: timed out");
        assert_eq!(unreachable, "Could not send through smtp.gmail.com:465: Connection error: timed out");
    }

    #[test]
    fn a_google_app_password_pasted_in_its_four_groups_loses_the_spaces() {
        let typed = |password: &str| super::password(&EmailSettings { password: password.into(), ..settings("me@gmail.com", "") });

        assert_eq!(typed(" abcd efgh  ijkl mnop "), "abcdefghijklmnop");
        assert_eq!(typed("my own pass word"), "my own pass word", "any other password is left as typed");
        assert_eq!(typed("abcd efgh ijkl"), "abcd efgh ijkl");
    }

    #[test]
    fn email_is_on_only_when_recipient_sender_and_password_are_all_set() {
        assert!(settings("me@qq.com", "").is_configured());
        assert!(!EmailSettings { password: " ".into(), ..settings("me@qq.com", "") }.is_configured());
        assert!(!EmailSettings::default().is_configured());
    }

    #[test]
    fn the_message_carries_the_minutes_as_text_and_as_html() {
        let message = minutes_message(
            &settings("me@qq.com", ""),
            "周会 <Q3>",
            "2026-09-20 10:30",
            "## 决定\n\n| Action | Owner |\n|---|---|\n| Send deck | unassigned |",
        )
        .unwrap();
        let raw = String::from_utf8(message.formatted()).unwrap();

        assert!(raw.contains("To: hui@example.com"));
        assert!(raw.contains("multipart/alternative"));
        assert!(raw.contains("text/plain") && raw.contains("text/html"));
    }

    #[test]
    fn the_subject_has_the_day_and_what_the_meeting_was_about() {
        let when = "2026-09-20 16:34";
        // The meeting was never named, and the model opened with the date: the first topic says more.
        let minutes = "# 2026年9月20日 会议\n\n## 1. 预算与收入测算\n**目标:**\n- ...\n\n## 2. 招聘\n";
        assert_eq!(subject("Meeting 2026-09-20 16:34", when, minutes), "Minutes 2026-09-20: 预算与收入测算");

        // A title the model wrote itself is the main topic.
        assert_eq!(subject("Meeting 2026-09-20 16:34", when, "# Q3 supplier review\n\n## 1. Prices\n"), "Minutes 2026-09-20: Q3 supplier review");
        // The user's own name for the meeting comes first.
        assert_eq!(subject("Board prep", when, minutes), "Minutes 2026-09-20: Board prep");
        // Section names say nothing either; with nothing better, the subject stays plain.
        assert_eq!(subject("Meeting 2026-09-20 16:34", when, "# Meeting 2026-09-20 16:34\n## Overview\n## **Pricing** for 2027\n"), "Minutes 2026-09-20: Pricing for 2027");
        assert_eq!(subject("Meeting 2026-09-20 16:34", when, "## Summary\nShort."), "Minutes 2026-09-20: Meeting");
        // The test mail has a sentence where the date goes.
        assert_eq!(subject("ZillaNote test", "If you can read this, minutes will arrive here.", "## It works"), "Minutes: ZillaNote test");
    }

    #[test]
    fn html_in_the_minutes_is_shown_not_run() {
        let html = render_html("## Plan\n\n<script>alert(1)</script> and <img src=x onerror=y>\n\n| A | B |\n|---|---|\n| 1 | 2 |");

        assert!(html.contains("<h2>Plan</h2>"));
        assert!(!html.contains("<script>") && !html.contains("<img"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("<td>1</td>"));
    }

    /// Talks to real mail servers with a password that cannot be right, so nothing is ever
    /// sent: it proves the connection, the encryption and the message a wrong password gets.
    ///
    /// cargo test -p engine live_smtp -- --ignored --nocapture
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs the network"]
    async fn live_smtp_rejects_a_wrong_password_with_a_helpful_message() {
        for from in ["zillanote-test@gmail.com", "zillanote-test@qq.com", "zillanote-test@outlook.com"] {
            let mut wrong = settings(from, "");
            wrong.password = "definitely-not-the-password".to_string();

            let error = send_test(&wrong).await.unwrap_err();

            println!("{from}: {error}\n");
            assert!(error.contains("did not accept the address or password"), "{from}: {error}");
        }
    }

    #[test]
    fn a_bad_address_is_reported_before_anything_is_sent() {
        let mut bad = settings("me@qq.com", "");
        bad.to = "not-an-address".to_string();

        assert!(minutes_message(&bad, "T", "now", "x").unwrap_err().contains("not-an-address"));
    }
}
