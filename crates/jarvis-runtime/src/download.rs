//! Verified downloads: SHA-256 checked while streaming, resumable `.part`
//! files, disk space check, progress and cancellation.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use futures::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::error::{Result, RuntimeError};
use crate::library::now_ms;
use crate::types::{DownloadKind, DownloadPhase, DownloadStatus};

/// Hosts downloads may start from. Redirects (CDNs) are followed; integrity
/// is guaranteed by the expected SHA-256, not by the host.
pub const ALLOWED_HOSTS: &[&str] = &["huggingface.co", "github.com"];
const KEEP_FINISHED: usize = 10;
const DISK_RESERVE: u64 = 256 * 1024 * 1024;

#[derive(Debug)]
struct Slot {
    status: DownloadStatus,
    cancel: CancellationToken,
}

/// Registry of downloads shown in the UI.
#[derive(Debug, Clone, Default)]
pub struct Downloads {
    slots: Arc<Mutex<Vec<Slot>>>,
}

impl Downloads {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Slot>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Registers a new download and returns its handle.
    pub fn begin(&self, kind: DownloadKind, label: impl Into<String>) -> DownloadHandle {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let cancel = CancellationToken::new();
        let mut slots = self.lock();
        // Drop the oldest finished entries.
        while slots
            .iter()
            .filter(|s| s.status.phase.is_finished())
            .count()
            >= KEEP_FINISHED
        {
            if let Some(index) = slots.iter().position(|s| s.status.phase.is_finished()) {
                slots.remove(index);
            }
        }
        slots.push(Slot {
            status: DownloadStatus {
                id: id.clone(),
                kind,
                label: label.into(),
                received_bytes: 0,
                total_bytes: None,
                bytes_per_second: 0,
                phase: DownloadPhase::Downloading,
                error: None,
                started_at_ms: now_ms(),
            },
            cancel: cancel.clone(),
        });
        DownloadHandle {
            id,
            cancel,
            downloads: self.clone(),
        }
    }

    pub fn snapshot(&self) -> Vec<DownloadStatus> {
        self.lock().iter().map(|s| s.status.clone()).collect()
    }

    pub fn has_active(&self, kind: DownloadKind) -> bool {
        self.lock()
            .iter()
            .any(|s| s.status.kind == kind && !s.status.phase.is_finished())
    }

    /// Cancels a running download. Returns `false` if it is not running.
    pub fn cancel(&self, id: &str) -> bool {
        let slots = self.lock();
        match slots.iter().find(|s| s.status.id == id) {
            Some(slot) if !slot.status.phase.is_finished() => {
                slot.cancel.cancel();
                true
            }
            _ => false,
        }
    }

    pub fn cancel_all(&self) {
        for slot in self.lock().iter() {
            slot.cancel.cancel();
        }
    }

    fn update(&self, id: &str, update: impl FnOnce(&mut DownloadStatus)) {
        if let Some(slot) = self.lock().iter_mut().find(|s| s.status.id == id) {
            update(&mut slot.status);
        }
    }
}

/// Progress reporting for one download.
#[derive(Debug, Clone)]
pub struct DownloadHandle {
    id: String,
    cancel: CancellationToken,
    downloads: Downloads,
}

impl DownloadHandle {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn cancel_token(&self) -> &CancellationToken {
        &self.cancel
    }

    pub fn progress(&self, received: u64, total: Option<u64>, bytes_per_second: u64) {
        self.downloads.update(&self.id, |s| {
            s.received_bytes = received;
            s.total_bytes = total;
            s.bytes_per_second = bytes_per_second;
        });
    }

    pub fn phase(&self, phase: DownloadPhase) {
        self.downloads.update(&self.id, |s| s.phase = phase);
    }

    /// Records the outcome of the whole job.
    pub fn finish(&self, result: &Result<()>) {
        self.downloads.update(&self.id, |s| match result {
            Ok(()) => s.phase = DownloadPhase::Completed,
            Err(RuntimeError::Cancelled) => s.phase = DownloadPhase::Cancelled,
            Err(err) => {
                s.phase = DownloadPhase::Failed;
                s.error = Some(err.to_string());
            }
        });
    }
}

/// What the downloaded file must match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    /// Lowercase hex SHA-256.
    pub sha256: String,
    pub size: Option<u64>,
}

/// Checks that a download URL starts at an allowed host over HTTPS.
pub fn check_source(url: &Url) -> Result<()> {
    let allowed = url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|host| ALLOWED_HOSTS.contains(&host));
    if allowed {
        Ok(())
    } else {
        Err(RuntimeError::PermissionDenied(format!(
            "downloads are only allowed from {} over HTTPS",
            ALLOWED_HOSTS.join(", ")
        )))
    }
}

/// Downloads `url` to `dest`, verifying size and SHA-256. Resumes a previous
/// `.part` file when the server supports ranges. On integrity failure the
/// partial file is deleted; on cancellation it is deleted as well.
pub async fn fetch_verified(
    client: &reqwest::Client,
    url: &Url,
    dest: &Path,
    expected: &Expected,
    handle: &DownloadHandle,
) -> Result<()> {
    let result = fetch_inner(client, url, dest, expected, handle).await;
    if matches!(
        result,
        Err(RuntimeError::Cancelled | RuntimeError::IntegrityMismatch { .. })
    ) {
        let _ = tokio::fs::remove_file(part_path(dest)).await;
    }
    result
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".part");
    dest.with_file_name(name)
}

async fn fetch_inner(
    client: &reqwest::Client,
    url: &Url,
    dest: &Path,
    expected: &Expected,
    handle: &DownloadHandle,
) -> Result<()> {
    let cancel = handle.cancel_token();
    let part = part_path(dest);
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| RuntimeError::io("creating the download directory", err))?;
    }

    // Resume: hash what is already there.
    let mut hasher = Sha256::new();
    let mut existing = match tokio::fs::metadata(&part).await {
        Ok(meta) if expected.size.is_none_or(|size| meta.len() < size) => meta.len(),
        _ => {
            let _ = tokio::fs::remove_file(&part).await;
            0
        }
    };
    if existing > 0 {
        hash_file_into(&part, &mut hasher, cancel).await?;
    }

    if let Some(size) = expected.size
        && let Some(parent) = dest.parent()
        && let Some(free) = jarvis_platform::available_disk_space(parent)
        && free < size.saturating_sub(existing) + DISK_RESERVE
    {
        return Err(RuntimeError::InsufficientDisk {
            needed_mib: (size - existing + DISK_RESERVE) / (1024 * 1024),
            available_mib: free / (1024 * 1024),
        });
    }

    let mut request = client.get(url.clone());
    if existing > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={existing}-"));
    }
    let response = tokio::select! {
        () = cancel.cancelled() => return Err(RuntimeError::Cancelled),
        response = request.send() => response.map_err(|err| RuntimeError::Download(err.to_string()))?,
    };
    let status = response.status();
    if existing > 0 && status == reqwest::StatusCode::OK {
        // The server ignored the range: start over.
        existing = 0;
        hasher = Sha256::new();
    } else if !status.is_success() {
        return Err(RuntimeError::Download(format!("server answered {status}")));
    }
    let total = expected
        .size
        .or_else(|| response.content_length().map(|len| len + existing));

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(existing > 0)
        .truncate(existing == 0)
        .open(&part)
        .await
        .map_err(|err| RuntimeError::io("opening the download file", err))?;

    let mut received = existing;
    let started = Instant::now();
    let mut last_report = Instant::now();
    let mut body = response.bytes_stream();
    loop {
        let chunk = tokio::select! {
            () = cancel.cancelled() => return Err(RuntimeError::Cancelled),
            chunk = body.next() => chunk,
        };
        let Some(chunk) = chunk else { break };
        let chunk = chunk.map_err(|err| RuntimeError::Download(err.to_string()))?;
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|err| RuntimeError::io("writing the download", err))?;
        received += chunk.len() as u64;
        if let Some(size) = expected.size
            && received > size
        {
            return Err(RuntimeError::Download(
                "the server sent more data than expected".into(),
            ));
        }
        if last_report.elapsed().as_millis() >= 250 {
            let secs = started.elapsed().as_secs_f64().max(0.001);
            let speed = ((received - existing) as f64 / secs) as u64;
            handle.progress(received, total, speed);
            last_report = Instant::now();
        }
    }
    file.flush()
        .await
        .map_err(|err| RuntimeError::io("writing the download", err))?;
    file.sync_all()
        .await
        .map_err(|err| RuntimeError::io("writing the download", err))?;
    drop(file);
    handle.progress(received, total, 0);

    handle.phase(DownloadPhase::Verifying);
    if let Some(size) = expected.size
        && received != size
    {
        return Err(RuntimeError::Download(format!(
            "incomplete download: {received} of {size} bytes"
        )));
    }
    let actual = hex(&hasher.finalize());
    if actual != expected.sha256.to_ascii_lowercase() {
        return Err(RuntimeError::IntegrityMismatch {
            file: dest
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            expected: expected.sha256.clone(),
            actual,
        });
    }
    tokio::fs::rename(&part, dest)
        .await
        .map_err(|err| RuntimeError::io("finishing the download", err))
}

async fn hash_file_into(
    path: &Path,
    hasher: &mut Sha256,
    cancel: &CancellationToken,
) -> Result<()> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|err| RuntimeError::io("reading a partial download", err))?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if cancel.is_cancelled() {
            return Err(RuntimeError::Cancelled);
        }
        let read = file
            .read(&mut buf)
            .await
            .map_err(|err| RuntimeError::io("reading a partial download", err))?;
        if read == 0 {
            return Ok(());
        }
        hasher.update(&buf[..read]);
    }
}

/// SHA-256 of a file as lowercase hex.
pub async fn sha256_file(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    hash_file_into(path, &mut hasher, &CancellationToken::new()).await?;
    Ok(hex(&hasher.finalize()))
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_from_allowed_hosts() {
        let ok = Url::parse("https://huggingface.co/a/b/resolve/main/x.gguf").expect("url");
        assert!(check_source(&ok).is_ok());
        for bad in [
            "http://huggingface.co/x",
            "https://example.com/x.gguf",
            "https://huggingface.co.evil.com/x",
            "file:///etc/passwd",
        ] {
            let url = Url::parse(bad).expect("url");
            assert!(check_source(&url).is_err(), "{bad}");
        }
    }

    #[test]
    fn hex_encodes_lowercase() {
        assert_eq!(hex(&[0x00, 0xab, 0xff]), "00abff");
        assert_eq!(
            hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn registry_tracks_and_cancels_downloads() {
        let downloads = Downloads::default();
        let handle = downloads.begin(DownloadKind::Model, "model");
        assert!(downloads.has_active(DownloadKind::Model));
        handle.progress(10, Some(100), 5);
        assert_eq!(downloads.snapshot()[0].received_bytes, 10);

        assert!(downloads.cancel(handle.id()));
        assert!(handle.cancel_token().is_cancelled());
        handle.finish(&Err(RuntimeError::Cancelled));
        assert_eq!(downloads.snapshot()[0].phase, DownloadPhase::Cancelled);
        assert!(
            !downloads.cancel(handle.id()),
            "finished downloads cannot be cancelled"
        );
        assert!(!downloads.has_active(DownloadKind::Model));
    }
}
