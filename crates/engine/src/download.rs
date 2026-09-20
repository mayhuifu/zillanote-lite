//! Fetches the models the app needs and does not ship: the speech model (2.8 GB) and the
//! two speaker models (32 MB).
//!
//! Every URL is pinned to a revision, with the size and checksum the file must have. A
//! download that stops (a dropped connection, the app closing, Cancel) leaves a `.part`
//! file that the next try continues from, because nobody wants the first gigabyte twice.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use qwen3_asr::Qwen3AsrModel;
use tokio::io::AsyncWriteExt;

use crate::pipeline::MODEL;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadFile {
    pub file_name: &'static str,
    pub url: &'static str,
    pub size_bytes: u64,
    /// CRC-32 (IEEE) of the whole file.
    pub crc32: u32,
}

/// From the commit of fastrepl/anarlog that `crates/speakers` was taken from.
pub const SPEAKER_MODELS: &[DownloadFile] = &[
    DownloadFile {
        file_name: "segmentation.onnx",
        url: "https://raw.githubusercontent.com/fastrepl/anarlog/b9146a707f4da2121d3438bce38155b31e5007f1/crates/pyannote-local/src/data/segmentation.onnx",
        size_bytes: 5_986_908,
        crc32: 2_837_126_254,
    },
    DownloadFile {
        file_name: "embedding.onnx",
        url: "https://raw.githubusercontent.com/fastrepl/anarlog/b9146a707f4da2121d3438bce38155b31e5007f1/crates/embedding/src/onnx/embedding.onnx",
        size_bytes: 26_543_975,
        crc32: 3_203_350_236,
    },
];

pub fn speech_model_files(model: Qwen3AsrModel) -> Vec<DownloadFile> {
    let files = model.downloads().iter();
    files
        .map(|file| DownloadFile {
            file_name: file.file_name,
            url: file.url,
            size_bytes: file.size_bytes,
            crc32: file.crc32,
        })
        .collect()
}

/// Files to fetch into one folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub dir: PathBuf,
    pub files: Vec<DownloadFile>,
}

/// What is missing, smallest first, so speaker names work long before the speech model
/// has arrived.
pub fn missing_packages(models_dir: &Path, speaker_models_dir: &Path) -> Vec<Package> {
    let mut packages = Vec::new();
    if speakers::SpeakerModels::locate(speaker_models_dir).is_none() {
        packages.push(Package {
            dir: speaker_models_dir.to_path_buf(),
            files: SPEAKER_MODELS.to_vec(),
        });
    }
    if MODEL.locate_files(models_dir).is_none() {
        packages.push(Package {
            dir: MODEL.install_dir(models_dir),
            files: speech_model_files(MODEL),
        });
    }
    packages
}

pub fn total_bytes(packages: &[Package]) -> u64 {
    packages.iter().flat_map(|package| &package.files).map(|file| file.size_bytes).sum()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Progress {
    pub done_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Retry {
    /// Tries in a row that may fail without a single new byte before giving up.
    pub attempts: usize,
    pub pause: Duration,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            attempts: 5,
            pause: Duration::from_secs(3),
        }
    }
}

pub const CANCELLED: &str = "The download was stopped. It continues from here next time.";

/// Downloads every file of every package that is not there yet. `on_progress` sees the
/// bytes of all of them together, files already in place included.
pub async fn download(
    packages: &[Package],
    retry: Retry,
    cancel: &AtomicBool,
    on_progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<(), String> {
    let total_bytes = total_bytes(packages);
    let mut before = 0;
    for package in packages {
        tokio::fs::create_dir_all(&package.dir)
            .await
            .map_err(|e| format!("{}: {e}", package.dir.display()))?;
        for file in &package.files {
            let report = |file_bytes: u64| {
                on_progress(Progress {
                    done_bytes: before + file_bytes,
                    total_bytes,
                })
            };
            fetch_file(file, &package.dir, retry, cancel, &report).await?;
            before += file.size_bytes;
        }
    }
    Ok(())
}

async fn fetch_file(
    file: &DownloadFile,
    dir: &Path,
    retry: Retry,
    cancel: &AtomicBool,
    report: &(dyn Fn(u64) + Send + Sync),
) -> Result<(), String> {
    let target = dir.join(file.file_name);
    let part = dir.join(format!("{}.part", file.file_name));
    if file_len(&target).await == Some(file.size_bytes) {
        report(file.size_bytes);
        return Ok(());
    }

    let mut failures = 0;
    loop {
        let had = file_len(&part).await.unwrap_or(0);
        match fetch_into(file, &part, cancel, report).await {
            Ok(()) => break,
            Err(error) if error == CANCELLED => return Err(error),
            Err(error) => {
                // Only tries that got nowhere count: a slow line that keeps dropping still arrives.
                failures = if file_len(&part).await.unwrap_or(0) > had { 1 } else { failures + 1 };
                if failures >= retry.attempts.max(1) {
                    return Err(format!("{} could not be downloaded: {error}", file.file_name));
                }
                tracing::warn!(file = file.file_name, %error, failures, "download_retry");
                tokio::time::sleep(retry.pause * failures as u32).await;
            }
        }
    }

    let checked = part.clone();
    let actual = tokio::task::spawn_blocking(move || crc32_of(&checked))
        .await
        .map_err(|e| e.to_string())??;
    if actual != file.crc32 {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(format!("{} arrived damaged (checksum mismatch). Try again.", file.file_name));
    }
    tokio::fs::rename(&part, &target)
        .await
        .map_err(|e| format!("{}: {e}", target.display()))
}

/// One connection's worth: appends to `part` from where it ends until the file is whole.
async fn fetch_into(
    file: &DownloadFile,
    part: &Path,
    cancel: &AtomicBool,
    report: &(dyn Fn(u64) + Send + Sync),
) -> Result<(), String> {
    let mut have = file_len(part).await.unwrap_or(0);
    if have > file.size_bytes {
        let _ = tokio::fs::remove_file(part).await;
        have = 0;
    }
    if have == file.size_bytes {
        return Ok(());
    }

    let mut client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(60));
    if crate::minutes::is_loopback(file.url) {
        client = client.no_proxy();
    }
    let client = client.build().map_err(|e| e.to_string())?;
    let mut request = client.get(file.url);
    if have > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let mut response = request.send().await.map_err(describe)?;
    if !response.status().is_success() {
        return Err(format!("the server answered {}", response.status()));
    }
    // A server that ignores the range sends the file from the start.
    if have > 0 && response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        have = 0;
    }

    let mut output = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(have > 0)
        .truncate(have == 0)
        .open(part)
        .await
        .map_err(|e| format!("{}: {e}", part.display()))?;
    report(have);
    while let Some(bytes) = response.chunk().await.map_err(describe)? {
        if cancel.load(Ordering::SeqCst) {
            output.flush().await.map_err(|e| e.to_string())?;
            return Err(CANCELLED.to_string());
        }
        output.write_all(&bytes).await.map_err(|e| format!("{}: {e}", part.display()))?;
        have += bytes.len() as u64;
        report(have.min(file.size_bytes));
    }
    output.flush().await.map_err(|e| e.to_string())?;

    match have {
        have if have == file.size_bytes => Ok(()),
        have if have < file.size_bytes => Err("the connection closed early".to_string()),
        _ => {
            let _ = tokio::fs::remove_file(part).await;
            Err("the server sent more than the file holds".to_string())
        }
    }
}

fn describe(error: reqwest::Error) -> String {
    if error.is_connect() {
        "no connection to the server".to_string()
    } else if error.is_timeout() {
        "the server stopped answering".to_string()
    } else {
        error.to_string()
    }
}

async fn file_len(path: &Path) -> Option<u64> {
    tokio::fs::metadata(path).await.ok().map(|metadata| metadata.len())
}

fn crc32_of(path: &Path) -> Result<u32, String> {
    use std::io::Read;

    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = crc32fast::Hasher::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        match file.read(&mut buffer).map_err(|e| e.to_string())? {
            0 => return Ok(hasher.finalize()),
            read => hasher.update(&buffer[..read]),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    use super::*;

    const WEIGHTS: &[u8] = b"these bytes stand in for two gigabytes of weights";

    /// `url` has to outlive the test as the real, compiled-in ones do.
    fn file(server: &MockServer, crc32: u32) -> DownloadFile {
        DownloadFile {
            file_name: "weights.gguf",
            url: Box::leak(format!("{}/weights.gguf", server.uri()).into_boxed_str()),
            size_bytes: WEIGHTS.len() as u64,
            crc32,
        }
    }

    fn package(dir: &Path, file: DownloadFile) -> Vec<Package> {
        vec![Package {
            dir: dir.to_path_buf(),
            files: vec![file],
        }]
    }

    fn quick() -> Retry {
        Retry {
            attempts: 3,
            pause: Duration::from_millis(1),
        }
    }

    /// Serves the whole file, or the rest of it when asked for a range.
    fn serve(request: &Request) -> ResponseTemplate {
        match request.headers.get("range").and_then(|range| range.to_str().ok()) {
            Some(range) => {
                let from = range.trim_start_matches("bytes=").trim_end_matches('-').parse::<usize>().unwrap();
                ResponseTemplate::new(206).set_body_bytes(&WEIGHTS[from..])
            }
            None => ResponseTemplate::new(200).set_body_bytes(WEIGHTS),
        }
    }

    #[tokio::test]
    async fn a_file_arrives_under_its_name_with_rising_progress() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/weights.gguf")).respond_with(serve).mount(&server).await;
        let dir = tempfile::tempdir().unwrap();
        let seen = Mutex::new(Vec::new());

        let packages = package(&dir.path().join("models"), file(&server, crc32fast::hash(WEIGHTS)));
        download(&packages, quick(), &AtomicBool::new(false), &|progress| seen.lock().unwrap().push(progress))
            .await
            .unwrap();

        assert_eq!(std::fs::read(dir.path().join("models/weights.gguf")).unwrap(), WEIGHTS);
        assert!(!dir.path().join("models/weights.gguf.part").exists());
        let seen = seen.into_inner().unwrap();
        assert!(seen.windows(2).all(|pair| pair[0].done_bytes <= pair[1].done_bytes));
        assert_eq!(seen.last().unwrap().done_bytes, WEIGHTS.len() as u64);
    }

    #[tokio::test]
    async fn a_download_that_stopped_half_way_continues_from_there() {
        let server = MockServer::start().await;
        let ranges = std::sync::Arc::new(Mutex::new(Vec::new()));
        let asked = ranges.clone();
        Mock::given(method("GET"))
            .respond_with(move |request: &Request| {
                let range = request.headers.get("range").map(|range| range.to_str().unwrap().to_string());
                asked.lock().unwrap().push(range);
                serve(request)
            })
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("weights.gguf.part"), &WEIGHTS[..20]).unwrap();

        let packages = package(dir.path(), file(&server, crc32fast::hash(WEIGHTS)));
        download(&packages, quick(), &AtomicBool::new(false), &|_| {}).await.unwrap();

        assert_eq!(*ranges.lock().unwrap(), [Some("bytes=20-".to_string())]);
        assert_eq!(std::fs::read(dir.path().join("weights.gguf")).unwrap(), WEIGHTS);
    }

    #[tokio::test]
    async fn a_file_already_in_place_is_not_fetched_again() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).respond_with(serve).expect(0).mount(&server).await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("weights.gguf"), WEIGHTS).unwrap();

        let packages = package(dir.path(), file(&server, crc32fast::hash(WEIGHTS)));
        download(&packages, quick(), &AtomicBool::new(false), &|_| {}).await.unwrap();
    }

    #[tokio::test]
    async fn a_damaged_file_fails_and_is_not_left_behind() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).respond_with(serve).mount(&server).await;
        let dir = tempfile::tempdir().unwrap();

        let packages = package(dir.path(), file(&server, 12_345));
        let error = download(&packages, quick(), &AtomicBool::new(false), &|_| {}).await.unwrap_err();

        assert!(error.contains("damaged"), "{error}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn a_server_that_keeps_failing_is_given_up_on_with_the_reason() {
        let server = MockServer::start().await;
        let tries = std::sync::Arc::new(AtomicUsize::new(0));
        let counted = tries.clone();
        Mock::given(method("GET"))
            .respond_with(move |_: &Request| {
                counted.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(503)
            })
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();

        let packages = package(dir.path(), file(&server, crc32fast::hash(WEIGHTS)));
        let error = download(&packages, quick(), &AtomicBool::new(false), &|_| {}).await.unwrap_err();

        assert!(error.contains("weights.gguf") && error.contains("503"), "{error}");
        assert_eq!(tries.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn stopping_keeps_what_has_arrived_for_the_next_time() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).respond_with(serve).mount(&server).await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("weights.gguf.part"), &WEIGHTS[..20]).unwrap();

        let packages = package(dir.path(), file(&server, crc32fast::hash(WEIGHTS)));
        let error = download(&packages, quick(), &AtomicBool::new(true), &|_| {}).await.unwrap_err();

        assert_eq!(error, CANCELLED);
        assert_eq!(std::fs::read(dir.path().join("weights.gguf.part")).unwrap(), &WEIGHTS[..20]);
    }

    #[test]
    fn only_what_is_missing_is_asked_for_and_the_small_models_come_first() {
        let dir = tempfile::tempdir().unwrap();
        let speaker_models = dir.path().join("speakers");

        let packages = missing_packages(dir.path(), &speaker_models);
        assert_eq!(packages[0].dir, speaker_models);
        assert_eq!(total_bytes(&packages[..1]), 32_530_883);

        std::fs::create_dir_all(&speaker_models).unwrap();
        for file in SPEAKER_MODELS {
            std::fs::write(speaker_models.join(file.file_name), b"model").unwrap();
        }
        assert!(missing_packages(dir.path(), &speaker_models).iter().all(|package| package.dir != speaker_models));
    }

    /// Fetches the two speaker models (32 MB) from GitHub and checks them:
    ///
    /// cargo test -p engine live_download -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "needs the network, downloads 32 MB"]
    async fn live_download_of_the_speaker_models_matches_the_pinned_checksums() {
        let dir = tempfile::tempdir().unwrap();
        let packages = vec![Package {
            dir: dir.path().to_path_buf(),
            files: SPEAKER_MODELS.to_vec(),
        }];
        let started = std::time::Instant::now();

        download(&packages, Retry::default(), &AtomicBool::new(false), &|_| {}).await.unwrap();

        println!("{} bytes in {:?}", total_bytes(&packages), started.elapsed());
        assert!(speakers::SpeakerModels::locate(dir.path()).is_some());
    }
}
