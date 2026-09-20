use std::io::Cursor;
use std::time::Duration;

use crate::{Error, Qwen3AsrModel, strip_asr_prefix};

pub const SAMPLE_RATE: u32 = 16_000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

/// Talks to the OpenAI-style transcription endpoint of a local `llama-server`.
#[derive(Debug, Clone)]
pub struct Qwen3AsrClient {
    http: reqwest::Client,
    endpoint: String,
    model: Qwen3AsrModel,
    prompt: Option<String>,
}

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
            endpoint: format!("{}/audio/transcriptions", base_url.trim_end_matches('/')),
            model,
            prompt: None,
        })
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
        let file = reqwest::multipart::Part::bytes(wav)
            .file_name("chunk.wav")
            .mime_str("audio/wav")
            .map_err(|e| Error::Request(e.to_string()))?;
        let mut form = reqwest::multipart::Form::new()
            .part("file", file)
            .text("model", self.model.as_str());
        if let Some(prompt) = &self.prompt {
            form = form.text("prompt", prompt.clone());
        }

        let response = self
            .http
            .post(&self.endpoint)
            .multipart(form)
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
