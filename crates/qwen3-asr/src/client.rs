use std::io::Cursor;
use std::time::Duration;

use crate::{Error, Qwen3AsrModel, strip_asr_prefix};

pub const SAMPLE_RATE: u32 = 16_000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

/// Talks to an OpenAI-style transcription endpoint: the local `llama-server` running
/// Qwen3-ASR, or a speech-recognition service the user has chosen instead.
#[derive(Debug, Clone)]
pub struct Qwen3AsrClient {
    http: reqwest::Client,
    endpoint: String,
    /// The model name the endpoint is asked for.
    model: String,
    api_key: Option<String>,
    prompt: Option<String>,
    /// Pauses before trying a chunk again after a passing failure; none for the local server,
    /// where a failure is not a busy network.
    retry_pauses: Vec<Duration>,
}

/// A hosted service can be busy or out of reach for a moment; a meeting's worth of chunks
/// should not lose one to that.
const SERVICE_RETRY_PAUSES: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(6)];

#[derive(serde::Deserialize)]
struct TranscriptionResponse {
    #[serde(default)]
    text: String,
}

impl Qwen3AsrClient {
    /// `base_url` is the server's OpenAI root, for example `http://127.0.0.1:8080/v1`.
    pub fn new(base_url: &str, model: Qwen3AsrModel) -> Result<Self, Error> {
        // The server is on this machine; a system-wide proxy must not sit in between.
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| Error::Request(e.to_string()))?;

        Ok(Self {
            http,
            endpoint: endpoint(base_url),
            model: model.as_str().to_string(),
            api_key: None,
            prompt: None,
            retry_pauses: Vec::new(),
        })
    }

    /// A speech-recognition service with OpenAI's transcription API (OpenAI, Groq,
    /// SiliconFlow, a self-hosted Whisper server). It is reached like any web site, through
    /// the system's proxy if there is one; an empty key sends no authorization.
    pub fn service(base_url: &str, model: &str, api_key: &str) -> Result<Self, Error> {
        let mut http = reqwest::Client::builder().timeout(REQUEST_TIMEOUT);
        // A server of the user's own on this computer is reached directly, as ours is.
        if is_on_this_computer(base_url) {
            http = http.no_proxy();
        }
        let http = http.build().map_err(|e| Error::Request(e.to_string()))?;
        let api_key = api_key.trim();
        Ok(Self {
            http,
            endpoint: endpoint(base_url),
            model: model.trim().to_string(),
            api_key: (!api_key.is_empty()).then(|| api_key.to_string()),
            prompt: None,
            retry_pauses: SERVICE_RETRY_PAUSES.to_vec(),
        })
    }

    /// Other pauses between tries, for tests that should not wait seconds.
    pub fn with_retry_pauses(mut self, pauses: &[Duration]) -> Self {
        self.retry_pauses = pauses.to_vec();
        self
    }

    /// Words the recognizer should prefer, such as names and product terms.
    pub fn with_vocabulary(mut self, vocabulary: &[String]) -> Self {
        let terms = vocabulary
            .iter()
            .map(|term| term.trim())
            .filter(|term| !term.is_empty())
            .collect::<Vec<_>>();
        self.prompt = (!terms.is_empty()).then(|| terms.join(", "));
        self
    }

    /// Keep chunks short: llama.cpp returns empty text for audio longer than about two
    /// minutes, and the model returns no timestamps to place words inside a long chunk.
    pub async fn transcribe_samples(&self, samples: &[f32]) -> Result<String, Error> {
        let wav = encode_wav(samples)?;
        let mut pauses = self.retry_pauses.iter();
        loop {
            match self.send(&wav).await {
                Err(error) if error.is_passing() => match pauses.next() {
                    Some(pause) => {
                        tracing::warn!(%error, "transcription_retried");
                        tokio::time::sleep(*pause).await;
                    }
                    None => return Err(error),
                },
                result => return result,
            }
        }
    }

    async fn send(&self, wav: &[u8]) -> Result<String, Error> {
        let file = reqwest::multipart::Part::bytes(wav.to_vec())
            .file_name("chunk.wav")
            .mime_str("audio/wav")
            .map_err(|e| Error::Request(e.to_string()))?;
        let mut form = reqwest::multipart::Form::new()
            .part("file", file)
            .text("model", self.model.clone());
        if let Some(prompt) = &self.prompt {
            form = form.text("prompt", prompt.clone());
        }

        let mut request = self.http.post(&self.endpoint).multipart(form);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        let response = request
            .send()
            .await
            .map_err(|e| Error::Request(e.to_string()))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| Error::Request(e.to_string()))?;
        if !status.is_success() {
            return Err(Error::Server {
                status: status.as_u16(),
                body: body.chars().take(300).collect(),
            });
        }

        let parsed: TranscriptionResponse =
            serde_json::from_str(&body).map_err(|e| Error::Request(e.to_string()))?;
        Ok(strip_asr_prefix(&parsed.text))
    }
}

fn is_on_this_computer(base_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url.trim()) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    // An IPv6 address comes in brackets.
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn endpoint(base_url: &str) -> String {
    format!("{}/audio/transcriptions", base_url.trim().trim_end_matches('/'))
}

fn encode_wav(samples: &[f32]) -> Result<Vec<u8>, Error> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
    {
        let mut writer = hound::WavWriter::new(Cursor::new(&mut bytes), spec)
            .map_err(|e| Error::Audio(e.to_string()))?;
        for sample in samples {
            let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
            writer
                .write_sample(value)
                .map_err(|e| Error::Audio(e.to_string()))?;
        }
        writer.finalize().map_err(|e| Error::Audio(e.to_string()))?;
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    use super::*;

    // The multipart body carries binary audio, so it cannot be matched as a string.
    fn body_contains(needle: &'static str) -> impl Fn(&Request) -> bool {
        move |request: &Request| {
            request
                .body
                .windows(needle.len())
                .any(|window| window == needle.as_bytes())
        }
    }

    fn one_second_of_silence() -> Vec<f32> {
        vec![0.0; SAMPLE_RATE as usize]
    }

    #[tokio::test]
    async fn posts_a_wav_chunk_and_returns_clean_text() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .and(body_contains("name=\"model\""))
            .and(body_contains("qwen3-asr-1.7b"))
            .and(body_contains("RIFF"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "type": "transcript.text.done",
                "text": "language Chinese<asr_text>好的，明白了。",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            Qwen3AsrClient::new(&format!("{}/v1/", server.uri()), Qwen3AsrModel::Large).unwrap();
        let text = client
            .transcribe_samples(&one_second_of_silence())
            .await
            .unwrap();

        assert_eq!(text, "好的，明白了。");
    }

    #[tokio::test]
    async fn sends_vocabulary_as_the_prompt() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_contains("name=\"prompt\""))
            .and(body_contains("ZillaNote, 付辉"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "text": "ok" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = Qwen3AsrClient::new(&format!("{}/v1", server.uri()), Qwen3AsrModel::Large)
            .unwrap()
            .with_vocabulary(&["ZillaNote".to_string(), " ".to_string(), "付辉".to_string()]);

        assert_eq!(
            client
                .transcribe_samples(&one_second_of_silence())
                .await
                .unwrap(),
            "ok"
        );
    }

    #[tokio::test]
    async fn reports_the_server_error_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("model failed to load"))
            .mount(&server)
            .await;

        let client =
            Qwen3AsrClient::new(&format!("{}/v1", server.uri()), Qwen3AsrModel::Large).unwrap();
        let error = client
            .transcribe_samples(&one_second_of_silence())
            .await
            .unwrap_err();

        assert!(
            matches!(error, Error::Server { status: 500, .. }),
            "{error}"
        );
        assert!(error.to_string().contains("model failed to load"));
    }

    #[tokio::test]
    async fn a_service_gets_its_model_name_and_the_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .and(wiremock::matchers::header("authorization", "Bearer sk-test"))
            .and(body_contains("whisper-large-v3"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "text": " Hello. " })))
            .expect(1)
            .mount(&server)
            .await;

        let client = Qwen3AsrClient::service(&format!(" {}/v1/ ", server.uri()), " whisper-large-v3 ", " sk-test ").unwrap();

        assert_eq!(client.transcribe_samples(&one_second_of_silence()).await.unwrap(), "Hello.");
    }

    #[tokio::test]
    async fn a_service_without_a_key_sends_no_authorization() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(|request: &Request| {
                let status = if request.headers.contains_key("authorization") { 400 } else { 200 };
                ResponseTemplate::new(status).set_body_json(serde_json::json!({ "text": "ok" }))
            })
            .mount(&server)
            .await;

        let client = Qwen3AsrClient::service(&format!("{}/v1", server.uri()), "whisper", "").unwrap();

        assert_eq!(client.transcribe_samples(&one_second_of_silence()).await.unwrap(), "ok");
    }

    #[tokio::test]
    async fn a_busy_service_is_tried_again_and_a_refusal_is_not() {
        let busy = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).set_body_string("slow down"))
            .up_to_n_times(1)
            .mount(&busy)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "text": "second try" })))
            .mount(&busy)
            .await;
        let client = Qwen3AsrClient::service(&format!("{}/v1", busy.uri()), "whisper", "key")
            .unwrap()
            .with_retry_pauses(&[Duration::from_millis(1)]);
        assert_eq!(client.transcribe_samples(&one_second_of_silence()).await.unwrap(), "second try");

        let refusing = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_string("invalid api key"))
            .expect(1)
            .mount(&refusing)
            .await;
        let client = Qwen3AsrClient::service(&format!("{}/v1", refusing.uri()), "whisper", "wrong")
            .unwrap()
            .with_retry_pauses(&[Duration::from_millis(1)]);
        let error = client.transcribe_samples(&one_second_of_silence()).await.unwrap_err();
        assert!(error.to_string().contains("invalid api key"), "{error}");
    }

    #[test]
    fn only_addresses_on_this_computer_skip_the_proxy() {
        for url in ["http://localhost:8000/v1", "http://127.0.0.1:9000/v1", "http://[::1]:8000/v1", " HTTP://LocalHost/v1 "] {
            assert!(is_on_this_computer(url), "{url}");
        }
        for url in ["https://api.openai.com/v1", "http://192.168.1.20:8000/v1", "not a url", ""] {
            assert!(!is_on_this_computer(url), "{url}");
        }
    }

    #[test]
    fn encodes_sixteen_bit_mono_wav() {
        let bytes = encode_wav(&[0.0, 1.0, -1.0, 2.0]).unwrap();
        let reader = hound::WavReader::new(Cursor::new(bytes)).unwrap();

        assert_eq!(reader.spec().channels, 1);
        assert_eq!(reader.spec().sample_rate, SAMPLE_RATE);
        let samples = reader
            .into_samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(samples, [0, i16::MAX, -i16::MAX, i16::MAX]);
    }
}
