//! Verified downloads against a local HTTP server.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jarvis_runtime::download::{Downloads, Expected, fetch_verified, hex};
use jarvis_runtime::{DownloadKind, DownloadPhase, RuntimeError};
use sha2::{Digest, Sha256};
use url::Url;

struct Server {
    url: Url,
    ranges: Arc<Mutex<Vec<String>>>,
}

/// Serves `content`; honours `Range` when `ranges` is true; sleeps between
/// 64 KiB chunks when `slow` is true.
fn serve(content: Vec<u8>, support_ranges: bool, slow: bool) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = Url::parse(&format!(
        "http://{}/model.gguf",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let ranges = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&ranges);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (content, seen) = (content.clone(), Arc::clone(&seen));
            std::thread::spawn(move || {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut range_start = None;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("range:") {
                        seen.lock().unwrap().push(value.trim().to_owned());
                        range_start = value
                            .trim()
                            .strip_prefix("bytes=")
                            .and_then(|r| r.trim_end_matches('-').parse::<usize>().ok());
                    }
                }
                let (status, body) = match range_start {
                    Some(start) if support_ranges => ("206 Partial Content", &content[start..]),
                    _ => ("200 OK", &content[..]),
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                for chunk in body.chunks(64 * 1024) {
                    if stream.write_all(chunk).is_err() {
                        return;
                    }
                    if slow {
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
            });
        }
    });
    Server { url, ranges }
}

fn content() -> Vec<u8> {
    (0..300_000u32).map(|i| (i % 251) as u8).collect()
}

fn expected(content: &[u8]) -> Expected {
    Expected {
        sha256: hex(&Sha256::digest(content)),
        size: Some(content.len() as u64),
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_and_verifies_files() {
    let data = content();
    let server = serve(data.clone(), true, false);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("model.gguf");
    let downloads = Downloads::default();
    let handle = downloads.begin_model("model".into(), dest.clone()).unwrap();

    fetch_verified(&client(), &server.url, &dest, &expected(&data), &handle)
        .await
        .expect("download");
    handle.finish(&Ok(()));
    assert_eq!(std::fs::read(&dest).unwrap(), data);
    let status = &downloads.snapshot()[0];
    assert_eq!(status.phase, DownloadPhase::Completed);
    assert_eq!(status.received_bytes, data.len() as u64);
    assert!(downloads.begin_model("New transfer".into(), dest).is_ok());
}

#[tokio::test]
async fn an_existing_file_is_reused_only_when_its_bytes_match() {
    let data = content();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("model.gguf");
    let downloads = Downloads::default();
    // No HTTP request is needed to reuse an already verified completed file.
    let offline = Url::parse("http://127.0.0.1:9/unavailable").unwrap();
    std::fs::write(&dest, &data).unwrap();
    let handle = downloads.begin_model("Reuse".into(), dest.clone()).unwrap();
    let result = fetch_verified(&client(), &offline, &dest, &expected(&data), &handle).await;
    handle.finish(&result);
    result.unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), data);
    assert_eq!(downloads.snapshot()[0].phase, DownloadPhase::Completed);
    assert_eq!(downloads.snapshot()[0].received_bytes, data.len() as u64);

    // An equal-length but different model must survive the attempted download.
    let other: Vec<u8> = data.iter().map(|byte| byte ^ 0xff).collect();
    std::fs::write(&dest, &other).unwrap();
    let handle = downloads
        .begin_model("Conflict".into(), dest.clone())
        .unwrap();
    let result = fetch_verified(&client(), &offline, &dest, &expected(&data), &handle).await;
    handle.finish(&result);
    assert!(matches!(result, Err(RuntimeError::InvalidInput(_))));
    assert_eq!(std::fs::read(&dest).unwrap(), other);
    assert!(!dir.path().join("model.gguf.part").exists());
}

#[tokio::test]
async fn a_directory_at_the_destination_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("model.gguf");
    std::fs::create_dir(&dest).unwrap();
    std::fs::write(dest.join("keep.txt"), "Keep this directory").unwrap();
    let handle = Downloads::default()
        .begin_model("Conflict".into(), dest.clone())
        .unwrap();
    let result = fetch_verified(
        &client(),
        &Url::parse("http://127.0.0.1:9/unavailable").unwrap(),
        &dest,
        &expected(&content()),
        &handle,
    )
    .await;
    assert!(matches!(result, Err(RuntimeError::InvalidInput(_))));
    assert_eq!(
        std::fs::read_to_string(dest.join("keep.txt")).unwrap(),
        "Keep this directory"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_appearing_during_transfer_is_never_overwritten() {
    for identical in [false, true] {
        let data = content();
        let server = serve(data.clone(), true, true);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.gguf");
        let part = dir.path().join("model.gguf.part");
        let downloads = Downloads::default();
        let handle = downloads.begin_model("Model".into(), dest.clone()).unwrap();
        let wanted = expected(&data);
        let output = dest.clone();
        let pending = tokio::spawn(async move {
            let result = fetch_verified(&client(), &server.url, &output, &wanted, &handle).await;
            handle.finish(&result);
            result
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if std::fs::metadata(&part).is_ok_and(|metadata| metadata.len() > 0) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("transfer starts before the destination is occupied");
        let existing = if identical {
            data.clone()
        } else {
            vec![42; data.len()]
        };
        std::fs::write(&dest, &existing).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), pending)
            .await
            .unwrap()
            .unwrap();
        if identical {
            result.unwrap();
            assert!(!part.exists());
        } else {
            assert!(matches!(result, Err(RuntimeError::InvalidInput(_))));
            assert_eq!(std::fs::read(&part).unwrap(), data);
        }
        assert_eq!(std::fs::read(&dest).unwrap(), existing);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn checksum_mismatches_are_rejected_and_cleaned_up() {
    let data = content();
    let server = serve(data.clone(), true, false);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("model.gguf");
    let downloads = Downloads::default();
    let handle = downloads.begin_model("model".into(), dest.clone()).unwrap();
    let wrong = Expected {
        sha256: "00".repeat(32),
        size: Some(data.len() as u64),
    };

    let err = fetch_verified(&client(), &server.url, &dest, &wrong, &handle)
        .await
        .expect_err("mismatch");
    assert!(
        matches!(err, RuntimeError::IntegrityMismatch { .. }),
        "{err:?}"
    );
    assert!(!dest.exists());
    assert!(
        !dir.path().join("model.gguf.part").exists(),
        "partial file removed"
    );
    assert!(downloads.begin_model("Retry".into(), dest.clone()).is_err());
    handle.finish(&Err(err));
    assert_eq!(downloads.snapshot()[0].phase, DownloadPhase::Failed);

    let retry = downloads.begin_model("Retry".into(), dest.clone()).unwrap();
    let result = fetch_verified(&client(), &server.url, &dest, &expected(&data), &retry).await;
    retry.finish(&result);
    result.expect("retry with the correct checksum");
    assert_eq!(std::fs::read(&dest).unwrap(), data);
}

#[tokio::test(flavor = "multi_thread")]
async fn partial_downloads_resume_with_a_range_request() {
    let data = content();
    let server = serve(data.clone(), true, false);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("model.gguf");
    std::fs::write(dir.path().join("model.gguf.part"), &data[..100_000]).unwrap();
    let handle = Downloads::default().begin(DownloadKind::Model, "model");

    fetch_verified(&client(), &server.url, &dest, &expected(&data), &handle)
        .await
        .expect("resumed download");
    assert_eq!(std::fs::read(&dest).unwrap(), data);
    assert_eq!(
        *server.ranges.lock().unwrap(),
        vec!["bytes=100000-".to_owned()]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn servers_without_range_support_restart_the_download() {
    let data = content();
    let server = serve(data.clone(), false, false);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("model.gguf");
    std::fs::write(dir.path().join("model.gguf.part"), &data[..100_000]).unwrap();
    let handle = Downloads::default().begin(DownloadKind::Model, "model");

    fetch_verified(&client(), &server.url, &dest, &expected(&data), &handle)
        .await
        .expect("restarted download");
    assert_eq!(std::fs::read(&dest).unwrap(), data, "no duplicated bytes");
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_stops_the_download_and_removes_the_partial_file() {
    let data = content();
    let server = serve(data.clone(), true, true);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("model.gguf");
    let downloads = Downloads::default();
    let handle = downloads.begin(DownloadKind::Model, "model");

    let id = handle.id().to_owned();
    let canceller = downloads.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(canceller.cancel(&id));
    });
    let err = fetch_verified(&client(), &server.url, &dest, &expected(&data), &handle)
        .await
        .expect_err("cancelled");
    assert!(matches!(err, RuntimeError::Cancelled), "{err:?}");
    assert!(!dest.exists());
    assert!(!dir.path().join("model.gguf.part").exists());
}
