//! Fetches the models the app needs and does not ship: the speech model (2.8 GB) and the
//! two speaker models (32 MB).
//!
//! Every URL is pinned to a revision, with the size and SHA-256 the file must have. A
//! download that stops (a dropped connection, the app closing, Cancel) leaves a `.part`
//! file that the next try continues from, because nobody wants the first gigabyte twice.
//!
//! Hugging Face and GitHub cannot be reached from everywhere, so each file has a second
//! source, a public mirror of the same path. A mirror is not trusted: whatever it sends has
//! to have the pinned checksum, or it is thrown away.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use qwen3_asr::Qwen3AsrModel;
use tokio::io::AsyncWriteExt;


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadFile {
    pub file_name: &'static str,
    pub url: &'static str,
    pub size_bytes: u64,
    /// SHA-256 of the whole file, in hex.
    pub sha256: &'static str,
}

impl DownloadFile {
    /// Where to fetch it from, in order: the pinned URL, then the same path on a mirror.
    pub fn sources(&self) -> Vec<String> {
        let mirror = if let Some(path) = self.url.strip_prefix("https://huggingface.co/") {
            Some(format!("https://hf-mirror.com/{path}"))
        } else if let Some(path) = self.url.strip_prefix("https://raw.githubusercontent.com/") {
            // <owner>/<repo>/<commit>/<file> is <owner>/<repo>@<commit>/<file> over there.
            let mut parts = path.splitn(4, '/');
            match (parts.next(), parts.next(), parts.next(), parts.next()) {
                (Some(owner), Some(repo), Some(commit), Some(file)) => {
                    Some(format!("https://cdn.jsdelivr.net/gh/{owner}/{repo}@{commit}/{file}"))
                }
                _ => None,
            }
        } else {
            None
        };
        std::iter::once(self.url.to_string()).chain(mirror).collect()
    }
}

/// From the commit of fastrepl/anarlog that `crates/speakers` was taken from.
pub const SPEAKER_MODELS: &[DownloadFile] = &[
    DownloadFile {
        file_name: "segmentation.onnx",
        url: "https://raw.githubusercontent.com/fastrepl/anarlog/b9146a707f4da2121d3438bce38155b31e5007f1/crates/pyannote-local/src/data/segmentation.onnx",
        size_bytes: 5_986_908,
        sha256: "057ee564753071c0b09b5b611648b50ac188d50846bff5f01e9f7bbf1591ea25",
    },
    DownloadFile {
        file_name: "embedding.onnx",
        url: "https://raw.githubusercontent.com/fastrepl/anarlog/b9146a707f4da2121d3438bce38155b31e5007f1/crates/embedding/src/onnx/embedding.onnx",
        size_bytes: 26_543_975,
        sha256: "841645a9b5109369858ca89695e8b23ecfd4486b30bdd5cb3f99fef8d1c29205",
    },
];

fn as_download(file: qwen3_asr::Qwen3AsrDownload) -> DownloadFile {
    DownloadFile {
        file_name: file.file_name,
        url: file.url,
        size_bytes: file.size_bytes,
        sha256: file.sha256,
    }
}

/// Files to fetch into one folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub dir: PathBuf,
    pub files: Vec<DownloadFile>,
}

/// What is missing for `model`, smallest first, so speaker names work long before the
/// speech model has arrived. A file LM Studio already has is not fetched again.
pub fn missing_packages(models_dir: &Path, speaker_models_dir: &Path, model: Qwen3AsrModel) -> Vec<Package> {
    let mut packages = Vec::new();
    if speakers::SpeakerModels::locate(speaker_models_dir).is_none() {
        packages.push(Package {
            dir: speaker_models_dir.to_path_buf(),
            files: SPEAKER_MODELS.to_vec(),
        });
    }
    let missing = model.missing_downloads(models_dir);
    if !missing.is_empty() {
        packages.push(Package {
            dir: model.install_dir(models_dir),
            files: missing.into_iter().map(as_download).collect(),
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

    fetch_from(file, &file.sources(), &target, &part, retry, cancel, report).await
}

/// `sources` serve the same bytes; the first is preferred.
async fn fetch_from(
    file: &DownloadFile,
    sources: &[String],
    target: &Path,
    part: &Path,
    retry: Retry,
    cancel: &AtomicBool,
    report: &(dyn Fn(u64) + Send + Sync),
) -> Result<(), String> {
    let mut source = 0;
    let mut failures = 0;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(CANCELLED.to_string());
        }
        let had = file_len(part).await.unwrap_or(0);
        match fetch_into(file, &sources[source], part, cancel, report).await {
            Ok(()) => break,
            Err(error) if error == CANCELLED => return Err(error),
            Err(error) => {
                // Only tries that got nowhere count: a slow line that keeps dropping still
                // arrives. They are also the ones that send us to the other source; what has
                // arrived stays, since both serve the same bytes.
                let progressed = file_len(part).await.unwrap_or(0) > had;
                failures = if progressed { 1 } else { failures + 1 };
                if failures >= retry.attempts.max(1) * sources.len() {
                    return Err(format!("{} could not be downloaded: {error}", file.file_name));
                }
                if !progressed {
                    source = (source + 1) % sources.len();
                }
                tracing::warn!(file = file.file_name, %error, failures, next = %sources[source], "download_retry");
                tokio::select! {
                    _ = tokio::time::sleep(retry.pause * failures as u32) => {}
                    _ = stopped(cancel) => return Err(CANCELLED.to_string()),
                }
            }
        }
    }

    let checked = part.to_path_buf();
    let actual = tokio::task::spawn_blocking(move || sha256_of(&checked))
        .await
        .map_err(|e| e.to_string())??;
    if !actual.eq_ignore_ascii_case(file.sha256) {
        let _ = tokio::fs::remove_file(part).await;
        return Err(format!("{} arrived damaged (checksum mismatch). Try again.", file.file_name));
    }
    tokio::fs::rename(part, target)
        .await
        .map_err(|e| format!("{}: {e}", target.display()))
}

/// One connection's worth: appends to `part` from where it ends until the file is whole.
async fn fetch_into(
    file: &DownloadFile,
    url: &str,
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
    if crate::minutes::is_loopback(url) {
        client = client.no_proxy();
    }
    let client = client.build().map_err(|e| e.to_string())?;
    let mut request = client.get(url);
    if have > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    // Connecting can take half a minute on a bad line; Stop must not wait for it.
    let mut response = tokio::select! {
        response = request.send() => response.map_err(describe)?,
        _ = stopped(cancel) => return Err(CANCELLED.to_string()),
    };
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
    loop {
        let bytes = tokio::select! {
            bytes = response.chunk() => bytes.map_err(describe)?,
            _ = stopped(cancel) => {
                output.flush().await.map_err(|e| e.to_string())?;
                return Err(CANCELLED.to_string());
            }
        };
        let Some(bytes) = bytes else { break };
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

/// How fast this connection fetches model files right now, in bytes a second: the first few
/// megabytes of a real file, from whichever source answers. For the estimate shown next to
/// each model before anyone commits to gigabytes.
pub async fn probe_speed() -> Option<f64> {
    const SAMPLE: u64 = 4 << 20;
    let file = as_download(Qwen3AsrModel::SmallQ4.downloads()[0]);
    for url in file.sources() {
        let Ok(client) = reqwest::Client::builder().connect_timeout(Duration::from_secs(8)).build() else {
            continue;
        };
        let started = std::time::Instant::now();
        let request = client.get(&url).header(reqwest::header::RANGE, format!("bytes=0-{}", SAMPLE - 1));
        let Ok(Ok(mut response)) = tokio::time::timeout(Duration::from_secs(10), request.send()).await else {
            continue;
        };
        if !response.status().is_success() {
            continue;
        }
        let mut received = 0u64;
        // Twelve seconds at most: on a slow line what has come by then is measure enough.
        while let Ok(Ok(Some(bytes))) = tokio::time::timeout(Duration::from_secs(12), response.chunk()).await {
            received += bytes.len() as u64;
            if received >= SAMPLE || started.elapsed() > Duration::from_secs(12) {
                break;
            }
        }
        if received > 100_000 {
            return Some(received as f64 / started.elapsed().as_secs_f64());
        }
    }
    None
}

/// Returns once Stop has been asked for.
async fn stopped(cancel: &AtomicBool) {
    while !cancel.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(100)).await;
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

fn sha256_of(path: &Path) -> Result<String, String> {
    use sha2::Digest;
    use std::io::Read;

    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        match file.read(&mut buffer).map_err(|e| e.to_string())? {
            0 => return Ok(hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect()),
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
    fn file(server: &MockServer, sha256: &'static str) -> DownloadFile {
        DownloadFile {
            file_name: "weights.gguf",
            url: Box::leak(format!("{}/weights.gguf", server.uri()).into_boxed_str()),
            size_bytes: WEIGHTS.len() as u64,
            sha256,
        }
    }

    fn right() -> &'static str {
        use sha2::Digest;
        let hex = sha2::Sha256::digest(WEIGHTS).iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        Box::leak(hex.into_boxed_str())
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

        let packages = package(&dir.path().join("models"), file(&server, right()));
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

        let packages = package(dir.path(), file(&server, right()));
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

        let packages = package(dir.path(), file(&server, right()));
        download(&packages, quick(), &AtomicBool::new(false), &|_| {}).await.unwrap();
    }

    #[tokio::test]
    async fn a_damaged_file_fails_and_is_not_left_behind() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).respond_with(serve).mount(&server).await;
        let dir = tempfile::tempdir().unwrap();

        let packages = package(dir.path(), file(&server, "00"));
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

        let packages = package(dir.path(), file(&server, right()));
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

        let packages = package(dir.path(), file(&server, right()));
        let error = download(&packages, quick(), &AtomicBool::new(true), &|_| {}).await.unwrap_err();

        assert_eq!(error, CANCELLED);
        assert_eq!(std::fs::read(dir.path().join("weights.gguf.part")).unwrap(), &WEIGHTS[..20]);
    }

    #[tokio::test]
    async fn stopping_does_not_wait_for_a_server_that_is_slow_to_answer() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(WEIGHTS).set_delay(Duration::from_secs(30)))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let packages = package(dir.path(), file(&server, right()));
        let cancel = AtomicBool::new(false);
        let started = std::time::Instant::now();

        let (result, ()) = tokio::join!(download(&packages, quick(), &cancel, &|_| {}), async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            cancel.store(true, Ordering::SeqCst);
        });

        assert_eq!(result.unwrap_err(), CANCELLED);
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    }

    #[tokio::test]
    async fn when_the_first_source_is_out_of_reach_the_mirror_serves_the_same_file() {
        let (blocked, mirror) = (MockServer::start().await, MockServer::start().await);
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(503)).expect(1).mount(&blocked).await;
        Mock::given(method("GET")).respond_with(serve).mount(&mirror).await;
        let dir = tempfile::tempdir().unwrap();
        let sources = [format!("{}/weights.gguf", blocked.uri()), format!("{}/weights.gguf", mirror.uri())];
        let (target, part) = (dir.path().join("weights.gguf"), dir.path().join("weights.gguf.part"));

        let wanted = file(&blocked, right());
        fetch_from(&wanted, &sources, &target, &part, quick(), &AtomicBool::new(false), &|_| {}).await.unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), WEIGHTS);
    }

    #[tokio::test]
    async fn a_mirror_that_serves_something_else_is_not_believed() {
        let mirror = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(WEIGHTS.to_ascii_uppercase()))
            .mount(&mirror)
            .await;
        let dir = tempfile::tempdir().unwrap();

        let packages = package(dir.path(), file(&mirror, right()));
        let error = download(&packages, quick(), &AtomicBool::new(false), &|_| {}).await.unwrap_err();

        assert!(error.contains("damaged"), "{error}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn every_pinned_file_has_a_mirror_of_the_same_path() {
        let model = as_download(Qwen3AsrModel::Large.downloads()[0]);
        assert_eq!(model.sources()[1], model.url.replace("huggingface.co", "hf-mirror.com"));
        assert_eq!(
            SPEAKER_MODELS[0].sources()[1],
            "https://cdn.jsdelivr.net/gh/fastrepl/anarlog@b9146a707f4da2121d3438bce38155b31e5007f1/crates/pyannote-local/src/data/segmentation.onnx"
        );
    }

    #[test]
    fn only_what_is_missing_is_asked_for_and_the_small_models_come_first() {
        let dir = tempfile::tempdir().unwrap();
        let speaker_models = dir.path().join("speakers");

        let packages = missing_packages(dir.path(), &speaker_models, Qwen3AsrModel::Large);
        assert_eq!(packages[0].dir, speaker_models);
        assert_eq!(total_bytes(&packages[..1]), 32_530_883);

        std::fs::create_dir_all(&speaker_models).unwrap();
        for file in SPEAKER_MODELS {
            std::fs::write(speaker_models.join(file.file_name), b"model").unwrap();
        }
        assert!(missing_packages(dir.path(), &speaker_models, Qwen3AsrModel::Large).iter().all(|package| package.dir != speaker_models));
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
