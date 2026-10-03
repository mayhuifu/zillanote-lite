//! Sign in with ChatGPT: the minutes written on the user's own ChatGPT plan, with no API key.
//!
//! This is OpenAI's path for open-source apps that run on the user's computer
//! (developers.openai.com/siwc/token-sharing-open-source). The browser signs in with OAuth and
//! PKCE and comes back to a one-time listener on 127.0.0.1. The first sign-in registers this
//! installation and is issued a client id of its own. A refresh token keeps the session, and it
//! is replaced every time it is used. Requests then go to the Responses API (`minutes.rs`).

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::store::Store;

/// Where the user sees and limits what ZillaNote uses of their plan.
pub const USAGE_URL: &str = "https://chatgpt.com/settings/usage";

/// The start of the failure recorded on a meeting when the plan's limit is reached; the
/// window recognizes it and offers "Manage usage".
pub const USAGE_LIMIT: &str = "Your ChatGPT plan has reached its usage limit.";

const NOT_SIGNED_IN: &str = "Sign in with ChatGPT in Settings → Language model first.";
const SIGN_IN_AGAIN: &str = "Your ChatGPT session is no longer valid. Sign in again in Settings → Language model.";
const NOT_GRANTED: &str = "ChatGPT plan access wasn't granted. Sign in again and allow it, or choose a model of your own in Settings → Language model.";

const ISSUER: &str = "https://auth.openai.com";
const RESOURCE: &str = "https://api.openai.com/v1";
const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
/// The scope that lets requests run on the plan. Signing in does not grant it by itself.
const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
/// The client id of a first sign-in. The answer carries the one issued to this installation,
/// and this one is never kept.
const REGISTER: &str = "dynamic_agent_client";
const APP_NAME: &str = "ZillaNote";
const CALLBACK_PATH: &str = "/auth/callback";
/// How long the browser has to finish the sign-in.
const SIGN_IN_TIME: Duration = Duration::from_secs(600);
/// A token closer than this to its end is renewed before a request, not halfway through one.
const RENEW_BEFORE_SECONDS: i64 = 300;
/// Refresh errors after which only a new sign-in helps.
const TERMINAL_REFRESH_ERRORS: [&str; 6] = [
    "invalid_grant",
    "invalid_refresh_token",
    "token_expired",
    "refresh_token_expired",
    "refresh_token_invalidated",
    "refresh_token_reused",
];

/// OpenAI's addresses; tests point them at a mock server.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub authorize: String,
    pub token: String,
    /// The API root that `/models` and `/responses` hang off.
    pub api: String,
}

impl Endpoints {
    pub fn openai() -> Self {
        Self {
            authorize: format!("{ISSUER}/api/accounts/authorize"),
            token: format!("{ISSUER}/api/accounts/oauth/token"),
            api: RESOURCE.to_string(),
        }
    }
}

/// What is kept about ChatGPT: `chatgpt.json`, with the tokens where secrets are kept
/// (see [`Store::chatgpt`]).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ChatGpt {
    /// `ext_agent_host_id`: this installation, the same for every sign-in.
    pub host_id: String,
    /// The one-time note "You're using your ChatGPT plan" has been shown.
    pub welcomed: bool,
    pub account: Option<Account>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<Tokens>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Account {
    pub email: String,
    /// The ID token's `sub`: who this is, whatever their email.
    pub subject: String,
    /// Issued to this installation at the first sign-in (`oaiapp_…`), for every renewal and
    /// for signing in again.
    pub client_id: String,
    /// A renewal names the address the sign-in came back to.
    pub redirect_uri: String,
    /// The user let ZillaNote use their plan.
    pub plan_usage: bool,
    /// The tokens stopped working for good (revoked, expired): only signing in again helps.
    pub needs_sign_in: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds.
    pub expires_at: i64,
}

/// What the window shows.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Status {
    pub signed_in: bool,
    pub email: Option<String>,
    pub plan_usage: bool,
    pub welcomed: bool,
}

impl ChatGpt {
    pub fn status(&self) -> Status {
        let account = self.account.as_ref();
        Status {
            signed_in: account.is_some_and(|account| !account.needs_sign_in),
            email: account.map(|account| account.email.clone()).filter(|email| !email.is_empty()),
            plan_usage: account.is_some_and(|account| account.plan_usage),
            welcomed: self.welcomed,
        }
    }

    /// Whether minutes can be asked for. Looks at the file only: no keychain.
    pub fn ready(&self) -> bool {
        let status = self.status();
        status.signed_in && status.plan_usage
    }
}

/// One of the models the account may use.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Model {
    pub slug: String,
    pub display_name: String,
}

// --- signing in ---

/// Signs in through the browser and keeps the account. `open_browser` is handed the page to
/// show. Signing in again after a session ended reuses the issued client id and the email.
pub async fn sign_in(
    store: &Store,
    endpoints: &Endpoints,
    open_browser: impl FnOnce(&str) -> Result<(), String>,
) -> Result<Status, String> {
    let mut saved = store.chatgpt();
    if saved.host_id.is_empty() {
        saved.host_id = new_host_id();
        store.save_chatgpt(&saved)?;
    }
    let previous = saved.account.clone().filter(|account| account.needs_sign_in);
    let signed_in = authorize(endpoints, &saved.host_id, previous.as_ref(), open_browser).await?;
    tracing::info!(plan_usage = signed_in.account.plan_usage, "chatgpt_signed_in");
    saved.account = Some(signed_in.account);
    saved.tokens = Some(signed_in.tokens);
    store.save_chatgpt(&saved)?;
    Ok(saved.status())
}

/// Forgets the account and its tokens. This installation's id and the one-time note stay.
pub fn sign_out(store: &Store) -> Result<(), String> {
    let mut saved = store.chatgpt();
    saved.account = None;
    saved.tokens = None;
    store.save_chatgpt(&saved)
}

pub fn mark_welcomed(store: &Store) -> Result<(), String> {
    let mut saved = store.chatgpt();
    saved.welcomed = true;
    store.save_chatgpt(&saved)
}

struct SignedIn {
    account: Account,
    tokens: Tokens,
}

async fn authorize(
    endpoints: &Endpoints,
    host_id: &str,
    previous: Option<&Account>,
    open_browser: impl FnOnce(&str) -> Result<(), String>,
) -> Result<SignedIn, String> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|error| format!("Could not wait for the browser: {error}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");

    let (verifier, state, nonce) = (random_token(), random_token(), random_token());
    let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
    let client_id = previous
        .map(|account| account.client_id.as_str())
        .filter(|id| !id.is_empty())
        .unwrap_or(REGISTER);

    let mut params = vec![
        ("client_id", client_id),
        ("ext_agent_host_id", host_id),
        ("response_type", "code"),
        ("redirect_uri", &redirect_uri),
        ("scope", SCOPE),
        ("resource", RESOURCE),
        ("state", &state),
        ("nonce", &nonce),
        ("code_challenge_method", "S256"),
        ("code_challenge", &challenge),
    ];
    if client_id == REGISTER {
        params.push(("agent_name_hint", APP_NAME));
    }
    if let Some(email) = previous.map(|account| account.email.as_str()).filter(|email| !email.is_empty()) {
        params.push(("login_hint", email));
    }
    let url = reqwest::Url::parse_with_params(&endpoints.authorize, &params).map_err(|e| e.to_string())?;
    open_browser(url.as_str())?;

    let answer = tokio::time::timeout(SIGN_IN_TIME, wait_for_callback(&listener))
        .await
        .map_err(|_| "The sign-in was not finished in the browser in time. Try again.".to_string())??;
    if answer.get("state") != Some(&state) {
        return Err("The browser came back from a different sign-in. Try again.".to_string());
    }
    if let Some(error) = answer.get("error") {
        return Err(if error == "access_denied" {
            "The sign-in was cancelled in the browser.".to_string()
        } else {
            format!("ChatGPT did not sign you in: {error}")
        });
    }
    let code = answer.get("code").ok_or("The browser came back without a sign-in code. Try again.")?;
    let client_id = answer.get("client_id").map(String::as_str).unwrap_or(client_id);
    if client_id == REGISTER {
        return Err("ChatGPT did not register ZillaNote. Try again.".to_string());
    }

    let now = unix_now();
    let tokens = token_request(
        &endpoints.token,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("code_verifier", &verifier),
            ("redirect_uri", &redirect_uri),
            ("resource", RESOURCE),
        ],
    )
    .await
    .map_err(|failure| failure.message)?;
    let id_token = tokens.id_token.as_deref().ok_or("ChatGPT did not say who signed in.")?;
    let identity = check_id_token(id_token, client_id, &nonce, now)?;
    let refresh_token = tokens
        .refresh_token
        .clone()
        .filter(|token| !token.is_empty())
        .ok_or("ChatGPT did not grant a lasting sign-in.")?;
    let scope = tokens.scope.as_deref().or(answer.get("scope").map(String::as_str)).unwrap_or_default();

    Ok(SignedIn {
        account: Account {
            email: identity.email,
            subject: identity.subject,
            client_id: client_id.to_string(),
            redirect_uri,
            plan_usage: scope.split_whitespace().any(|granted| granted == PLAN_SCOPE),
            needs_sign_in: false,
        },
        tokens: Tokens {
            access_token: tokens.access_token,
            refresh_token,
            expires_at: now + tokens.expires_in.unwrap_or(3600),
        },
    })
}

/// Answers whatever the browser asks for until it brings the sign-in back, and returns that
/// request's query.
async fn wait_for_callback(listener: &tokio::net::TcpListener) -> Result<HashMap<String, String>, String> {
    loop {
        let (mut socket, _) = listener.accept().await.map_err(|e| e.to_string())?;
        // A connection that says nothing must not hold up the one that matters.
        let Ok(Ok(head)) = tokio::time::timeout(Duration::from_secs(10), read_request_head(&mut socket)).await else {
            continue;
        };
        let target = head.split_whitespace().nth(1).unwrap_or_default();
        let Ok(url) = reqwest::Url::parse(&format!("http://127.0.0.1{target}")) else {
            continue;
        };
        if url.path() != CALLBACK_PATH {
            let _ = socket.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
            continue;
        }
        let page = "<!doctype html><meta charset=\"utf-8\"><title>ZillaNote</title>\
                    <body style=\"font:16px -apple-system,system-ui,sans-serif;margin:3em\">\
                    <p>You can close this tab and go back to ZillaNote.</p></body>";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
            page.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.shutdown().await;
        return Ok(url.query_pairs().into_owned().collect());
    }
}

async fn read_request_head(socket: &mut tokio::net::TcpStream) -> std::io::Result<String> {
    let mut head = Vec::new();
    let mut buffer = [0u8; 4096];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") && head.len() < 64 * 1024 {
        let read = socket.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        head.extend_from_slice(&buffer[..read]);
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

#[derive(Debug)]
struct Identity {
    subject: String,
    email: String,
}

/// The ID token's claims. Its signature is not checked: the token came straight from OpenAI's
/// token endpoint over TLS, which OpenID Connect accepts in place of the signature for a token
/// received that way (Core 1.0, 3.1.3.7), and it saves a JWT library.
fn check_id_token(token: &str, client_id: &str, nonce: &str, now: i64) -> Result<Identity, String> {
    let invalid = |why: &str| format!("ChatGPT's answer about who signed in was not valid ({why}). Try again.");
    let payload = token.split('.').nth(1).ok_or_else(|| invalid("format"))?;
    let claims: serde_json::Value = base64url_decode(payload)
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or_else(|| invalid("format"))?;

    if claims["iss"].as_str().map(|issuer| issuer.trim_end_matches('/')) != Some(ISSUER) {
        return Err(invalid("issuer"));
    }
    let audience_matches = match &claims["aud"] {
        serde_json::Value::String(audience) => audience == client_id,
        serde_json::Value::Array(audiences) => audiences.iter().any(|audience| audience == client_id),
        _ => false,
    };
    if !audience_matches {
        return Err(invalid("audience"));
    }
    // A minute of leeway for a clock that is a little off.
    if claims["exp"].as_i64().is_none_or(|expires| expires + 60 < now) {
        return Err(invalid("expired"));
    }
    if claims["nonce"].as_str() != Some(nonce) {
        return Err(invalid("nonce"));
    }
    let subject = claims["sub"].as_str().filter(|sub| !sub.is_empty()).ok_or_else(|| invalid("subject"))?;
    Ok(Identity {
        subject: subject.to_string(),
        email: claims["email"].as_str().unwrap_or_default().to_string(),
    })
}

// --- tokens ---

#[derive(serde::Deserialize)]
struct TokenAnswer {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: Option<i64>,
    scope: Option<String>,
}

struct TokenFailure {
    message: String,
    /// Only a new sign-in helps.
    terminal: bool,
}

async fn token_request(url: &str, params: &[(&str, &str)]) -> Result<TokenAnswer, TokenFailure> {
    let passing = |message: String| TokenFailure { message, terminal: false };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| passing(e.to_string()))?;
    let response = client
        .post(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form_body(params))
        .send()
        .await
        .map_err(|error| passing(format!("ChatGPT could not be reached: {error}")))?;
    let status = response.status();
    let body = response.text().await.map_err(|error| passing(format!("ChatGPT's answer broke off: {error}")))?;
    if !status.is_success() {
        let code = error_code(&body).unwrap_or_default();
        return Err(TokenFailure {
            terminal: TERMINAL_REFRESH_ERRORS.contains(&code.as_str()) || status.as_u16() == 401,
            message: format!("ChatGPT answered {status}: {}", body.chars().take(300).collect::<String>()),
        });
    }
    serde_json::from_str(&body).map_err(|_| passing("ChatGPT's sign-in answer was not in the expected format.".to_string()))
}

/// `application/x-www-form-urlencoded`, with the encoder reqwest's URL type already has.
fn form_body(params: &[(&str, &str)]) -> String {
    let mut url = reqwest::Url::parse("http://form.invalid/").expect("a valid URL");
    url.query_pairs_mut().extend_pairs(params);
    url.query().unwrap_or_default().to_string()
}

/// The code in an OAuth error (`{"error": "invalid_grant"}`) or an API error
/// (`{"error": {"code": "…"}}`).
pub(crate) fn error_code(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let error = &value["error"];
    error
        .as_str()
        .or_else(|| error["code"].as_str())
        .or_else(|| error["type"].as_str())
        .or_else(|| value["code"].as_str())
        .map(str::to_string)
}

/// The refresh token works once: two renewals at the same time would end the session.
fn renewing() -> &'static tokio::sync::Mutex<()> {
    static RENEWING: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    RENEWING.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// A token for the next request, renewed first when it is about to run out.
pub async fn access_token(store: &Store, endpoints: &Endpoints) -> Result<String, String> {
    let _one_at_a_time = renewing().lock().await;
    let mut saved = store.chatgpt();
    let Some(account) = saved.account.clone() else {
        return Err(NOT_SIGNED_IN.to_string());
    };
    if account.needs_sign_in {
        return Err(SIGN_IN_AGAIN.to_string());
    }
    if !account.plan_usage {
        return Err(NOT_GRANTED.to_string());
    }
    let Some(tokens) = saved.tokens.clone() else {
        // The keychain was not readable, or lost the item.
        return Err(SIGN_IN_AGAIN.to_string());
    };
    let now = unix_now();
    if tokens.expires_at - now > RENEW_BEFORE_SECONDS {
        return Ok(tokens.access_token);
    }

    let answer = token_request(
        &endpoints.token,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &tokens.refresh_token),
            ("client_id", &account.client_id),
            ("resource", RESOURCE),
            ("redirect_uri", &account.redirect_uri),
        ],
    )
    .await;
    match answer {
        Ok(answer) => {
            let renewed = Tokens {
                access_token: answer.access_token,
                // It is replaced on every use; an answer without a new one keeps the old.
                refresh_token: answer.refresh_token.filter(|token| !token.is_empty()).unwrap_or(tokens.refresh_token),
                expires_at: now + answer.expires_in.unwrap_or(3600),
            };
            saved.tokens = Some(renewed.clone());
            store.save_chatgpt(&saved)?;
            Ok(renewed.access_token)
        }
        Err(failure) if failure.terminal => {
            tracing::warn!(error = %failure.message, "chatgpt_session_ended");
            saved.tokens = None;
            if let Some(account) = saved.account.as_mut() {
                account.needs_sign_in = true;
            }
            store.save_chatgpt(&saved)?;
            Err(SIGN_IN_AGAIN.to_string())
        }
        Err(failure) => Err(failure.message),
    }
}

/// The models the account may use, as OpenAI lists them for showing.
pub async fn models(endpoints: &Endpoints, access_token: &str) -> Result<Vec<Model>, String> {
    #[derive(serde::Deserialize)]
    struct Listed {
        slug: String,
        #[serde(default)]
        display_name: String,
        #[serde(default)]
        visibility: String,
    }
    #[derive(serde::Deserialize)]
    struct List {
        models: Vec<Listed>,
    }

    let url = format!("{}/models", endpoints.api.trim_end_matches('/'));
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?
        .get(&url)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|error| format!("ChatGPT could not be reached: {error}"))?;
    let status = response.status();
    let body = response.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        let code = error_code(&body).unwrap_or_default();
        return Err(plan_failure(&code)
            .map(|(message, _)| message)
            .unwrap_or_else(|| format!("ChatGPT answered {status}: {}", body.chars().take(300).collect::<String>())));
    }
    let list: List =
        serde_json::from_str(&body).map_err(|_| "ChatGPT's list of models was not in the expected format.".to_string())?;
    Ok(list
        .models
        .into_iter()
        .filter(|model| model.visibility == "list")
        .map(|model| Model {
            display_name: if model.display_name.is_empty() { model.slug.clone() } else { model.display_name },
            slug: model.slug,
        })
        .collect())
}

/// What a request refused on the user's plan means for them, and whether trying again a
/// little later can help. `None` for codes that are not about the plan.
pub(crate) fn plan_failure(code: &str) -> Option<(String, bool)> {
    let (message, passing) = match code {
        "subscription_sharing_usage_limit_exceeded" => (
            format!("{USAGE_LIMIT} Check your usage settings (Settings → Language model → Manage usage), then write the minutes again."),
            false,
        ),
        "subscription_sharing_usage_unavailable" | "subscription_sharing_user_unavailable" => {
            ("ChatGPT usage information is temporarily unavailable. Please try again in a moment.".to_string(), true)
        }
        "subscription_sharing_invalid_user" => (SIGN_IN_AGAIN.to_string(), false),
        "subscription_sharing_unsupported_capability" => {
            ("ZillaNote asked for something your ChatGPT plan does not include. Choose another model in Settings → Language model.".to_string(), false)
        }
        _ => return None,
    };
    Some((message, passing))
}

// --- small helpers ---

fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("the system's random numbers");
    bytes
}

/// 32 random bytes: a PKCE verifier, a state, a nonce.
fn random_token() -> String {
    base64url(&random_bytes::<32>())
}

/// A random (version 4) UUID, as OpenAI suggests for `ext_agent_host_id`.
fn new_host_id() -> String {
    let mut bytes = random_bytes::<16>();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("urn:uuid:{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

const BASE64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Base64url without padding, as PKCE and JWTs use it.
fn base64url(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, byte)| n | (u32::from(*byte) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            text.push(BASE64URL[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    text
}

fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::with_capacity(text.len() * 3 / 4);
    let (mut n, mut bits) = (0u32, 0);
    for c in text.trim_end_matches('=').bytes() {
        n = (n << 6) | BASE64URL.iter().position(|&known| known == c)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((n >> bits) as u8);
        }
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[test]
    fn base64url_matches_the_examples_of_its_rfc_and_reads_back() {
        // RFC 7636, appendix B: the verifier and the challenge made from it.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(base64url(&Sha256::digest(verifier.as_bytes())), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        for text in ["", "f", "fo", "foo", "foob", "fooba", "foobar"] {
            assert_eq!(base64url_decode(&base64url(text.as_bytes())).unwrap(), text.as_bytes());
        }
        assert_eq!(base64url(b"foobar"), "Zm9vYmFy");
        assert!(base64url_decode("not base64!").is_none());
    }

    #[test]
    fn a_host_id_is_a_random_uuid() {
        let id = new_host_id();
        assert_eq!(id.len(), "urn:uuid:".len() + 36, "{id}");
        assert_eq!(&id[23..24], "4", "{id}");
        assert_ne!(id, new_host_id());
    }

    fn id_token(claims: serde_json::Value) -> String {
        format!("{}.{}.signature", base64url(br#"{"alg":"RS256"}"#), base64url(claims.to_string().as_bytes()))
    }

    fn claims(client_id: &str, nonce: &str) -> serde_json::Value {
        serde_json::json!({
            "iss": ISSUER, "aud": [client_id], "exp": unix_now() + 3600, "nonce": nonce,
            "sub": "user-123", "email": "ada@example.com",
        })
    }

    #[test]
    fn an_id_token_counts_only_for_this_client_this_sign_in_and_while_it_lasts() {
        let now = unix_now();
        let identity = check_id_token(&id_token(claims("oaiapp_1", "n")), "oaiapp_1", "n", now).unwrap();
        assert_eq!((identity.subject.as_str(), identity.email.as_str()), ("user-123", "ada@example.com"));

        assert!(check_id_token(&id_token(claims("oaiapp_2", "n")), "oaiapp_1", "n", now).unwrap_err().contains("audience"));
        assert!(check_id_token(&id_token(claims("oaiapp_1", "other")), "oaiapp_1", "n", now).unwrap_err().contains("nonce"));
        let mut old = claims("oaiapp_1", "n");
        old["exp"] = serde_json::json!(now - 600);
        assert!(check_id_token(&id_token(old), "oaiapp_1", "n", now).unwrap_err().contains("expired"));
        let mut elsewhere = claims("oaiapp_1", "n");
        elsewhere["iss"] = serde_json::json!("https://example.com");
        assert!(check_id_token(&id_token(elsewhere), "oaiapp_1", "n", now).unwrap_err().contains("issuer"));
        assert!(check_id_token("garbage", "oaiapp_1", "n", now).is_err());
    }

    #[test]
    fn plan_errors_say_what_to_do_and_only_a_passing_one_is_retried() {
        let (limit, passing) = plan_failure("subscription_sharing_usage_limit_exceeded").unwrap();
        assert!(limit.starts_with(USAGE_LIMIT) && !passing);
        assert!(plan_failure("subscription_sharing_usage_unavailable").unwrap().1);
        assert!(!plan_failure("subscription_sharing_invalid_user").unwrap().1);
        assert_eq!(plan_failure("model_not_found"), None);
        assert_eq!(error_code(r#"{"error":{"code":"subscription_sharing_usage_limit_exceeded"}}"#).as_deref(), Some("subscription_sharing_usage_limit_exceeded"));
        assert_eq!(error_code(r#"{"error":"invalid_grant","error_description":"x"}"#).as_deref(), Some("invalid_grant"));
    }

    fn endpoints(server: &MockServer) -> Endpoints {
        Endpoints {
            authorize: format!("{}/authorize", server.uri()),
            token: format!("{}/token", server.uri()),
            api: format!("{}/v1", server.uri()),
        }
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().to_path_buf()).unwrap();
        (dir, store)
    }

    /// Stands in for the browser: reads what the sign-in page was asked, and comes back to the
    /// app's listener the way ChatGPT does, from another thread, as a browser would.
    fn browser(answer: impl Fn(&HashMap<String, String>) -> Vec<(&'static str, String)> + Send + 'static) -> impl FnOnce(&str) -> Result<(), String> {
        move |url: &str| {
            let asked: HashMap<String, String> = reqwest::Url::parse(url).unwrap().query_pairs().into_owned().collect();
            let mut back = reqwest::Url::parse(&asked["redirect_uri"]).unwrap();
            back.query_pairs_mut().extend_pairs(answer(&asked));
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                let client = reqwest::Client::builder().no_proxy().build().unwrap();
                runtime.block_on(async { client.get(back).send().await.unwrap().text().await.unwrap() });
            });
            Ok(())
        }
    }

    #[tokio::test]
    async fn signing_in_registers_this_installation_and_keeps_the_account_and_its_tokens() {
        let server = MockServer::start().await;
        let nonce = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let seen = nonce.clone();
        let (_dir, store) = store();

        // The token endpoint is mounted once the nonce is known, so its ID token can carry it.
        let open = browser(move |asked| {
            assert_eq!(asked["client_id"], REGISTER);
            assert_eq!(asked["agent_name_hint"], "ZillaNote");
            assert_eq!(asked["code_challenge_method"], "S256");
            assert!(asked["scope"].contains(PLAN_SCOPE) && asked["ext_agent_host_id"].starts_with("urn:uuid:"));
            *seen.lock().unwrap() = asked["nonce"].clone();
            vec![
                ("code", "the-code".to_string()),
                ("state", asked["state"].clone()),
                ("client_id", "oaiapp_1".to_string()),
            ]
        });
        let token = id_token_responder(nonce.clone());
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("client_id=oaiapp_1"))
            .and(body_string_contains("code_verifier="))
            .respond_with(token)
            .mount(&server)
            .await;

        let status = sign_in(&store, &endpoints(&server), open).await.unwrap();

        assert!(status.signed_in && status.plan_usage && !status.welcomed);
        assert_eq!(status.email.as_deref(), Some("ada@example.com"));
        let saved = store.chatgpt();
        assert_eq!(saved.account.as_ref().unwrap().client_id, "oaiapp_1");
        assert_eq!(saved.tokens.as_ref().unwrap().refresh_token, "refresh-1");
        assert!(saved.ready());
    }

    /// Answers the code exchange with an ID token for whatever nonce the sign-in used.
    fn id_token_responder(nonce: std::sync::Arc<std::sync::Mutex<String>>) -> impl wiremock::Respond {
        move |_: &wiremock::Request| {
            let nonce = nonce.lock().unwrap().clone();
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "access-1", "refresh_token": "refresh-1", "token_type": "Bearer",
                "expires_in": 3600, "scope": SCOPE, "id_token": id_token(claims("oaiapp_1", &nonce)),
            }))
        }
    }

    #[tokio::test]
    async fn a_sign_in_refused_in_the_browser_keeps_nothing() {
        let server = MockServer::start().await;
        let (_dir, store) = store();
        let open = browser(|asked| vec![("error", "access_denied".to_string()), ("state", asked["state"].clone())]);

        let error = sign_in(&store, &endpoints(&server), open).await.unwrap_err();

        assert!(error.contains("cancelled"), "{error}");
        assert_eq!(store.chatgpt().account, None);
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_answer_for_another_sign_in_is_turned_away() {
        let server = MockServer::start().await;
        let (_dir, store) = store();
        let open = browser(|_| vec![("code", "c".to_string()), ("state", "forged".to_string())]);

        let error = sign_in(&store, &endpoints(&server), open).await.unwrap_err();

        assert!(error.contains("different sign-in"), "{error}");
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    fn signed_in(store: &Store, expires_at: i64) {
        store
            .save_chatgpt(&ChatGpt {
                host_id: "urn:uuid:x".to_string(),
                welcomed: true,
                account: Some(Account {
                    email: "ada@example.com".to_string(),
                    subject: "user-123".to_string(),
                    client_id: "oaiapp_1".to_string(),
                    redirect_uri: "http://127.0.0.1:1455/auth/callback".to_string(),
                    plan_usage: true,
                    needs_sign_in: false,
                }),
                tokens: Some(Tokens {
                    access_token: "access-1".to_string(),
                    refresh_token: "refresh-1".to_string(),
                    expires_at,
                }),
            })
            .unwrap();
    }

    #[tokio::test]
    async fn a_fresh_token_is_used_as_it_is_and_one_about_to_run_out_is_renewed_and_kept() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=refresh-1"))
            .and(body_string_contains("client_id=oaiapp_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "access-2", "refresh_token": "refresh-2", "expires_in": 3600,
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (_dir, store) = store();

        signed_in(&store, unix_now() + 3000);
        assert_eq!(access_token(&store, &endpoints(&server)).await.unwrap(), "access-1");

        signed_in(&store, unix_now() + 60);
        assert_eq!(access_token(&store, &endpoints(&server)).await.unwrap(), "access-2");
        // The new refresh token replaces the used one: the old one works no more.
        assert_eq!(store.chatgpt().tokens.unwrap().refresh_token, "refresh-2");
        assert_eq!(access_token(&store, &endpoints(&server)).await.unwrap(), "access-2");
    }

    #[tokio::test]
    async fn a_session_that_ended_asks_for_a_new_sign_in_and_keeps_the_client_id_for_it() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({ "error": "refresh_token_reused" })))
            .mount(&server)
            .await;
        let (_dir, store) = store();
        signed_in(&store, unix_now() - 10);

        let error = access_token(&store, &endpoints(&server)).await.unwrap_err();

        assert!(error.contains("Sign in again"), "{error}");
        let saved = store.chatgpt();
        assert_eq!(saved.tokens, None);
        let account = saved.account.unwrap();
        assert!(account.needs_sign_in && account.client_id == "oaiapp_1");
        assert!(!store.chatgpt().status().signed_in);
    }

    #[tokio::test]
    async fn a_passing_trouble_renewing_keeps_the_session() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(503).set_body_string("busy"))
            .mount(&server)
            .await;
        let (_dir, store) = store();
        signed_in(&store, unix_now() - 10);

        assert!(access_token(&store, &endpoints(&server)).await.unwrap_err().contains("503"));
        assert!(store.chatgpt().status().signed_in);
        assert!(store.chatgpt().tokens.is_some());
    }

    #[tokio::test]
    async fn signing_out_forgets_the_account_but_not_the_installation() {
        let (_dir, store) = store();
        signed_in(&store, unix_now() + 3000);

        sign_out(&store).unwrap();

        let saved = store.chatgpt();
        assert_eq!((saved.account, saved.tokens), (None, None));
        assert_eq!(saved.host_id, "urn:uuid:x");
        assert!(saved.welcomed);
        assert_eq!(access_token(&store, &Endpoints::openai()).await.unwrap_err(), NOT_SIGNED_IN);
    }

    /// Opens the real sign-in page in the browser, then lists the account's models and asks
    /// the first one for a line of minutes, on a throwaway data folder:
    ///
    /// cargo test -p engine live_chatgpt -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "needs a person to sign in with ChatGPT in the browser"]
    async fn live_chatgpt_signs_in_lists_models_and_writes_a_line() {
        let (_dir, store) = store();
        let endpoints = Endpoints::openai();
        let status = sign_in(&store, &endpoints, |url| {
            println!("Signing in at {url}");
            std::process::Command::new("open").arg(url).spawn().map(|_| ()).map_err(|e| e.to_string())
        })
        .await
        .unwrap();
        println!("{status:?}");
        assert!(status.plan_usage, "ChatGPT plan use was not allowed");

        let token = access_token(&store, &endpoints).await.unwrap();
        let models = models(&endpoints, &token).await.unwrap();
        println!("{models:#?}");
        let config = crate::minutes::LlmConfig {
            api: crate::minutes::LlmApi::ChatGptPlan,
            base_url: endpoints.api.clone(),
            api_key: token,
            model: models[0].slug.clone(),
            max_chars_per_call: 24_000,
        };
        let minutes = crate::minutes::write_minutes(
            &config,
            "Answer in one short line.",
            crate::templates::template("discussion"),
            crate::minutes::About { title: "Test", ..Default::default() },
            "[00:00] Ada: We ship on Friday.\n[00:05] Bo: Agreed.",
            |_, _| {},
        )
        .await
        .unwrap();
        println!("{minutes}");
        assert!(!minutes.is_empty());
    }

    #[tokio::test]
    async fn only_the_models_meant_for_showing_are_listed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(wiremock::matchers::header("authorization", "Bearer access-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "models": [
                { "slug": "gpt-6.1-sol", "display_name": "GPT-6.1 Sol", "visibility": "list" },
                { "slug": "internal", "display_name": "Internal", "visibility": "hide" },
                { "slug": "gpt-mini", "visibility": "list" },
            ]})))
            .mount(&server)
            .await;

        let models = models(&endpoints(&server), "access-1").await.unwrap();

        assert_eq!(
            models,
            vec![
                Model { slug: "gpt-6.1-sol".to_string(), display_name: "GPT-6.1 Sol".to_string() },
                Model { slug: "gpt-mini".to_string(), display_name: "gpt-mini".to_string() },
            ]
        );
    }
}
