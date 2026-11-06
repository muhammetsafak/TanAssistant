//! `ModelManager` against a local HTTP server: verified downloads, resume, rejection of bad
//! files, listing and deletion.

#![allow(
    clippy::similar_names,
    clippy::cast_possible_truncation,
    clippy::case_sensitive_file_extension_comparisons
)]

use std::fmt::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tan_infra_transcription::{DownloadProgress, ModelError, ModelManager, ModelSpec};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hex = String::new();
    for byte in Sha256::digest(bytes) {
        write!(hex, "{byte:02x}").unwrap();
    }
    hex
}

/// A model whose catalog entry describes `body` exactly.
fn spec(id: &'static str, server: &MockServer, body: &[u8]) -> ModelSpec {
    ModelSpec {
        id,
        display_name: id,
        file_name: leak(format!("ggml-{id}.bin")),
        url: leak(format!("{}/{id}.bin", server.uri())),
        size_bytes: body.len() as u64,
        sha256: leak(sha256_hex(body)),
        quantization: "q5_1",
        recommended: false,
    }
}

fn body(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 % 251) as u8).collect()
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

async fn serve(server: &MockServer, url_path: &str, template: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(url_path))
        .respond_with(template)
        .mount(server)
        .await;
}

#[tokio::test]
async fn a_matching_download_is_installed_and_reports_progress() {
    let server = MockServer::start().await;
    let data = body(300_000);
    let model = spec("tiny", &server, &data);
    serve(
        &server,
        "/tiny.bin",
        ResponseTemplate::new(200).set_body_bytes(data.clone()),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let manager =
        ModelManager::with_catalog(dir.path().join("models"), vec![model.clone()]).unwrap();

    let events = Arc::new(Mutex::new(Vec::<DownloadProgress>::new()));
    let sink = events.clone();
    let installed = manager
        .download("tiny", move |p| sink.lock().unwrap().push(p))
        .await
        .unwrap();

    assert_eq!(std::fs::read(&installed).unwrap(), data);
    assert_eq!(
        files_in(manager.dir()),
        [model.file_name],
        "no .part left behind"
    );
    let events = events.lock().unwrap();
    assert_eq!(events.first().unwrap().downloaded, 0);
    let last = events.last().unwrap();
    assert_eq!((last.downloaded, last.total), (300_000, 300_000));
    assert!(events
        .windows(2)
        .all(|w| w[0].downloaded <= w[1].downloaded));
    assert!(manager.list()[0].installed);
    assert_eq!(manager.installed_path("tiny"), Some(installed));
}

#[tokio::test]
async fn a_checksum_mismatch_is_rejected_and_nothing_stays_on_disk() {
    let server = MockServer::start().await;
    let expected = body(10_000);
    let model = spec("bad", &server, &expected);
    // Same length, different bytes.
    let mut served = expected.clone();
    served[5_000] ^= 0xff;
    serve(
        &server,
        "/bad.bin",
        ResponseTemplate::new(200).set_body_bytes(served),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let manager = ModelManager::with_catalog(dir.path(), vec![model]).unwrap();

    let error = manager.download("bad", |_| {}).await.unwrap_err();

    assert!(matches!(error, ModelError::ChecksumMismatch(id) if id == "bad"));
    assert!(
        files_in(dir.path()).is_empty(),
        "{:?}",
        files_in(dir.path())
    );
    assert!(!manager.list()[0].installed);
    assert!(manager.default_installed().is_none());
}

#[tokio::test]
async fn a_server_error_leaves_nothing_behind() {
    let server = MockServer::start().await;
    let data = body(100);
    let model = spec("gone", &server, &data);
    serve(&server, "/gone.bin", ResponseTemplate::new(404)).await;
    let dir = tempfile::tempdir().unwrap();
    let manager = ModelManager::with_catalog(dir.path(), vec![model]).unwrap();

    let error = manager.download("gone", |_| {}).await.unwrap_err();

    assert!(matches!(error, ModelError::HttpStatus(404)));
    assert!(files_in(dir.path()).iter().all(|f| !f.ends_with(".bin")));
}

#[tokio::test]
async fn more_data_than_the_catalog_says_is_refused() {
    let server = MockServer::start().await;
    let data = body(1_000);
    let model = spec("big", &server, &data);
    serve(
        &server,
        "/big.bin",
        ResponseTemplate::new(200).set_body_bytes(body(5_000)),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let manager = ModelManager::with_catalog(dir.path(), vec![model]).unwrap();

    let error = manager.download("big", |_| {}).await.unwrap_err();

    assert!(matches!(error, ModelError::TooLarge));
    assert_eq!(files_in(dir.path()), Vec::<String>::new());
}

/// Serves `data`, honouring `Range: bytes=N-`.
struct RangeServer {
    data: Vec<u8>,
    saw_range: Arc<AtomicBool>,
    full_requests: Arc<AtomicUsize>,
    reject_ranges: bool,
}

impl Respond for RangeServer {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let Some(range) = request.headers.get("range") else {
            self.full_requests.fetch_add(1, Ordering::SeqCst);
            return ResponseTemplate::new(200).set_body_bytes(self.data.clone());
        };
        self.saw_range.store(true, Ordering::SeqCst);
        if self.reject_ranges {
            return ResponseTemplate::new(416);
        }
        let start: usize = range
            .to_str()
            .unwrap()
            .trim_start_matches("bytes=")
            .trim_end_matches('-')
            .parse()
            .unwrap();
        ResponseTemplate::new(206).set_body_bytes(self.data[start..].to_vec())
    }
}

#[tokio::test]
async fn an_interrupted_download_resumes_from_its_part_file() {
    let server = MockServer::start().await;
    let data = body(200_000);
    let model = spec("resume", &server, &data);
    let saw_range = Arc::new(AtomicBool::new(false));
    let full_requests = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/resume.bin"))
        .respond_with(RangeServer {
            data: data.clone(),
            saw_range: saw_range.clone(),
            full_requests: full_requests.clone(),
            reject_ranges: false,
        })
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join(format!("{}.part", model.file_name));
    std::fs::write(&part, &data[..120_000]).unwrap();
    let manager = ModelManager::with_catalog(dir.path(), vec![model.clone()]).unwrap();
    assert_eq!(manager.list()[0].partial_bytes, 120_000);

    let first_event = Arc::new(Mutex::new(None));
    let sink = first_event.clone();
    let installed = manager
        .download("resume", move |p| {
            sink.lock().unwrap().get_or_insert(p);
        })
        .await
        .unwrap();

    assert!(saw_range.load(Ordering::SeqCst), "a Range request was sent");
    assert_eq!(
        full_requests.load(Ordering::SeqCst),
        0,
        "the whole file was not fetched again"
    );
    assert_eq!(std::fs::read(installed).unwrap(), data);
    assert_eq!(first_event.lock().unwrap().unwrap().downloaded, 120_000);
    assert!(!part.exists());
}

#[tokio::test]
async fn a_stale_part_file_that_the_server_refuses_is_restarted() {
    let server = MockServer::start().await;
    let data = body(50_000);
    let model = spec("stale", &server, &data);
    let full_requests = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/stale.bin"))
        .respond_with(RangeServer {
            data: data.clone(),
            saw_range: Arc::default(),
            full_requests: full_requests.clone(),
            reject_ranges: true,
        })
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(format!("{}.part", model.file_name)),
        &data[..10_000],
    )
    .unwrap();
    let manager = ModelManager::with_catalog(dir.path(), vec![model]).unwrap();

    let installed = manager.download("stale", |_| {}).await.unwrap();

    assert_eq!(std::fs::read(installed).unwrap(), data);
    assert_eq!(full_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_corrupt_part_file_fails_verification_and_is_deleted() {
    let server = MockServer::start().await;
    let data = body(50_000);
    let model = spec("corrupt", &server, &data);
    Mock::given(method("GET"))
        .and(path("/corrupt.bin"))
        .respond_with(RangeServer {
            data: data.clone(),
            saw_range: Arc::default(),
            full_requests: Arc::default(),
            reject_ranges: false,
        })
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    // The first 10 kB on disk are not the model's: resuming on top of them cannot verify.
    std::fs::write(
        dir.path().join(format!("{}.part", model.file_name)),
        vec![7_u8; 10_000],
    )
    .unwrap();
    let manager = ModelManager::with_catalog(dir.path(), vec![model]).unwrap();

    let error = manager.download("corrupt", |_| {}).await.unwrap_err();

    assert!(matches!(error, ModelError::ChecksumMismatch(_)));
    assert_eq!(files_in(dir.path()), Vec::<String>::new());
    // Retrying starts clean and succeeds.
    manager.download("corrupt", |_| {}).await.unwrap();
}

#[tokio::test]
async fn a_short_transfer_keeps_the_part_file_for_resuming() {
    let server = MockServer::start().await;
    let data = body(40_000);
    let model = spec("short", &server, &data);
    serve(
        &server,
        "/short.bin",
        ResponseTemplate::new(200).set_body_bytes(data[..25_000].to_vec()),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let manager = ModelManager::with_catalog(dir.path(), vec![model.clone()]).unwrap();

    let error = manager.download("short", |_| {}).await.unwrap_err();

    assert!(matches!(
        error,
        ModelError::Incomplete {
            received: 25_000,
            expected: 40_000
        }
    ));
    assert_eq!(manager.list()[0].partial_bytes, 25_000);
    assert!(!manager.list()[0].installed);
}

#[tokio::test]
async fn an_installed_model_is_not_downloaded_again() {
    let server = MockServer::start().await; // no mocks: any request would 404
    let data = body(2_000);
    let model = spec("have", &server, &data);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(model.file_name), &data).unwrap();
    let manager = ModelManager::with_catalog(dir.path(), vec![model]).unwrap();

    manager.download("have", |_| {}).await.unwrap();

    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn two_downloads_of_one_model_do_not_run_at_once() {
    let server = MockServer::start().await;
    let data = body(1_000);
    let model = spec("busy", &server, &data);
    serve(
        &server,
        "/busy.bin",
        ResponseTemplate::new(200)
            .set_body_bytes(data)
            .set_delay(Duration::from_millis(400)),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let manager = ModelManager::with_catalog(dir.path(), vec![model]).unwrap();

    let (first, second) = tokio::join!(manager.download("busy", |_| {}), async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        manager.download("busy", |_| {}).await
    });

    assert!(first.is_ok());
    assert!(matches!(second, Err(ModelError::Busy(_))));
}

#[tokio::test]
async fn unknown_models_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let manager = ModelManager::with_catalog(dir.path(), Vec::new()).unwrap();
    assert!(matches!(
        manager.download("nope", |_| {}).await,
        Err(ModelError::UnknownModel(_))
    ));
    assert!(matches!(
        manager.delete("nope"),
        Err(ModelError::UnknownModel(_))
    ));
    assert!(manager.installed_path("nope").is_none());
}

#[tokio::test]
async fn delete_removes_the_model_and_its_part_file() {
    let server = MockServer::start().await;
    let data = body(500);
    let model = spec("del", &server, &data);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(model.file_name), &data).unwrap();
    std::fs::write(dir.path().join(format!("{}.part", model.file_name)), b"x").unwrap();
    std::fs::write(dir.path().join("unrelated.txt"), b"keep").unwrap();
    let manager = ModelManager::with_catalog(dir.path(), vec![model]).unwrap();
    assert!(manager.list()[0].installed);

    assert!(manager.delete("del").unwrap());

    assert_eq!(files_in(dir.path()), ["unrelated.txt"]);
    assert!(!manager.list()[0].installed);
    assert!(!manager.delete("del").unwrap(), "nothing left to remove");
}

#[tokio::test]
async fn the_most_preferred_installed_model_is_the_default() {
    let server = MockServer::start().await;
    let (a, b, c) = (body(100), body(200), body(300));
    let catalog = vec![
        spec("best", &server, &a),
        spec("middle", &server, &b),
        spec("small", &server, &c),
    ];
    let dir = tempfile::tempdir().unwrap();
    let manager = ModelManager::with_catalog(dir.path(), catalog.clone()).unwrap();
    assert!(manager.default_installed().is_none());

    std::fs::write(dir.path().join(catalog[2].file_name), &c).unwrap();
    assert_eq!(manager.default_installed().unwrap().spec.id, "small");

    std::fs::write(dir.path().join(catalog[1].file_name), &b).unwrap();
    assert_eq!(manager.default_installed().unwrap().spec.id, "middle");

    // A file with the wrong size is not an installed model (an interrupted copy).
    std::fs::write(dir.path().join(catalog[0].file_name), b"short").unwrap();
    assert_eq!(manager.default_installed().unwrap().spec.id, "middle");
}

#[test]
fn the_builtin_catalog_is_listed_and_nothing_is_installed_in_an_empty_directory() {
    let dir = tempfile::tempdir().unwrap();
    let manager = ModelManager::new(dir.path()).unwrap();
    assert_eq!(
        manager.list().len(),
        tan_infra_transcription::BUILTIN_MODELS.len()
    );
    assert!(manager.list().iter().all(|m| !m.installed));
    assert!(manager
        .list()
        .iter()
        .all(|m| m.spec.url.starts_with("https://")));
    assert!(manager.default_installed().is_none());
}

#[tokio::test]
async fn a_chosen_model_wins_over_the_catalog_order_until_it_is_gone() {
    let server = MockServer::start().await;
    let (a, b) = (body(100), body(200));
    let catalog = vec![spec("big", &server, &a), spec("small", &server, &b)];
    let dir = tempfile::tempdir().unwrap();
    let manager = ModelManager::with_catalog(dir.path(), catalog.clone()).unwrap();
    std::fs::write(dir.path().join(catalog[0].file_name), &a).unwrap();
    std::fs::write(dir.path().join(catalog[1].file_name), &b).unwrap();
    assert_eq!(manager.default_installed().unwrap().spec.id, "big");
    assert!(manager.list()[0].selected && !manager.list()[1].selected);

    manager.select("small").unwrap();

    assert_eq!(manager.default_installed().unwrap().spec.id, "small");
    assert!(!manager.list()[0].selected && manager.list()[1].selected);

    // Deleting the chosen model falls back to the most preferred one left.
    manager.delete("small").unwrap();
    assert_eq!(manager.default_installed().unwrap().spec.id, "big");
}

#[tokio::test]
async fn only_an_installed_known_model_can_be_chosen() {
    let server = MockServer::start().await;
    let data = body(100);
    let dir = tempfile::tempdir().unwrap();
    let manager =
        ModelManager::with_catalog(dir.path(), vec![spec("absent", &server, &data)]).unwrap();

    assert!(matches!(
        manager.select("absent"),
        Err(ModelError::NotInstalled(_))
    ));
    assert!(matches!(
        manager.select("nope"),
        Err(ModelError::UnknownModel(_))
    ));
    assert_eq!(files_in(dir.path()), Vec::<String>::new());
}
