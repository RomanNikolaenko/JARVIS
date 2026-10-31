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
    destination: Option<PathBuf>,
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
        self.register(&mut self.lock(), kind, label.into(), None)
    }

    /// Atomically reserves the output path, including its partial download file.
    pub fn begin_model(&self, label: String, destination: PathBuf) -> Result<DownloadHandle> {
        // Model file names are ASCII; Windows paths are case-insensitive.
        #[cfg(target_os = "windows")]
        let destination = PathBuf::from(destination.to_string_lossy().to_lowercase());
        let mut slots = self.lock();
        if slots.iter().any(|slot| {
            slot.destination.as_ref() == Some(&destination) && !slot.status.phase.is_finished()
        }) {
            return Err(RuntimeError::InvalidInput(format!(
                "{label} is already downloading to this file"
            )));
        }
        Ok(self.register(&mut slots, DownloadKind::Model, label, Some(destination)))
    }

    fn register(
        &self,
        slots: &mut Vec<Slot>,
        kind: DownloadKind,
        label: String,
        destination: Option<PathBuf>,
    ) -> DownloadHandle {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let cancel = CancellationToken::new();
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
                label,
                received_bytes: 0,
                total_bytes: None,
                bytes_per_second: 0,
                phase: DownloadPhase::Downloading,
                error: None,
                started_at_ms: now_ms(),
            },
            cancel: cancel.clone(),
            destination,
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
    if let Some(size) = verified_existing(dest, expected, cancel).await? {
        handle.phase(DownloadPhase::Verifying);
        handle.progress(size, Some(size), 0);
        return Ok(());
    }
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
    publish_verified(&part, dest, expected, cancel).await
}

/// A completed file may belong to another model or to an imported file. Reuse
/// it only after checking its actual bytes; never replace it based on its name.
async fn verified_existing(
    dest: &Path,
    expected: &Expected,
    cancel: &CancellationToken,
) -> Result<Option<u64>> {
    if cancel.is_cancelled() {
        return Err(RuntimeError::Cancelled);
    }
    let metadata = match tokio::fs::symlink_metadata(dest).await {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(RuntimeError::io("checking the download destination", err)),
    };
    let conflict = || {
        RuntimeError::InvalidInput(format!(
            "{} already exists and is not this verified download; move the existing file or remove its model and file before downloading a different file with the same name",
            dest.display()
        ))
    };
    if !metadata.file_type().is_file() || expected.size.is_some_and(|size| metadata.len() != size) {
        return Err(conflict());
    }
    let mut hasher = Sha256::new();
    hash_file_into(dest, &mut hasher, cancel).await?;
    if hex(&hasher.finalize()) != expected.sha256.to_ascii_lowercase() {
        return Err(conflict());
    }
    Ok(Some(metadata.len()))
}

/// A hard link publishes the verified file without replacing an existing path.
/// File systems without hard links use an exclusive create and copy instead.
async fn publish_verified(
    part: &Path,
    dest: &Path,
    expected: &Expected,
    cancel: &CancellationToken,
) -> Result<()> {
    if cancel.is_cancelled() {
        return Err(RuntimeError::Cancelled);
    }
    match tokio::fs::hard_link(part, dest).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            require_verified_existing(dest, expected, cancel).await?;
        }
        Err(_) => {
            // create_new is the no-overwrite boundary even if another program
            // put a file at this path while the HTTP transfer was in flight.
            let opened = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dest)
                .await;
            let mut output = match opened {
                Ok(output) => output,
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    require_verified_existing(dest, expected, cancel).await?;
                    let _ = tokio::fs::remove_file(part).await;
                    return Ok(());
                }
                Err(err) => return Err(RuntimeError::io("finishing the download", err)),
            };
            let copied = async {
                let mut input = tokio::fs::File::open(part)
                    .await
                    .map_err(|err| RuntimeError::io("reading the verified download", err))?;
                tokio::select! {
                    () = cancel.cancelled() => return Err(RuntimeError::Cancelled),
                    result = tokio::io::copy(&mut input, &mut output) => {
                        result.map_err(|err| RuntimeError::io("finishing the download", err))?;
                    }
                }
                output
                    .flush()
                    .await
                    .map_err(|err| RuntimeError::io("finishing the download", err))?;
                output
                    .sync_all()
                    .await
                    .map_err(|err| RuntimeError::io("finishing the download", err))
            }
            .await;
            drop(output);
            if let Err(err) = copied {
                let _ = tokio::fs::remove_file(dest).await;
                return Err(err);
            }
        }
    }
    let _ = tokio::fs::remove_file(part).await;
    Ok(())
}

async fn require_verified_existing(
    dest: &Path,
    expected: &Expected,
    cancel: &CancellationToken,
) -> Result<()> {
    verified_existing(dest, expected, cancel)
        .await?
        .ok_or_else(|| {
            RuntimeError::Download(
                "the destination changed while finishing the download; retry the download".into(),
            )
        })?;
    Ok(())
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
    fn model_destination_is_reserved_until_worker_finishes() {
        let downloads = Downloads::default();
        let path = PathBuf::from("models/model.gguf");
        let first = downloads
            .begin_model("Catalog label".into(), path.clone())
            .expect("first");
        assert!(
            downloads
                .begin_model("Different repository label".into(), path.clone())
                .is_err()
        );
        assert!(downloads.cancel(first.id()));
        assert!(
            downloads.begin_model("Retry".into(), path.clone()).is_err(),
            "worker may still write after cancellation is requested"
        );
        first.finish(&Err(RuntimeError::Cancelled));
        assert!(downloads.begin_model("Retry".into(), path).is_ok());
        assert!(
            downloads
                .begin_model("Independent".into(), PathBuf::from("models/other.gguf"))
                .is_ok()
        );
    }

    #[test]
    fn concurrent_model_reservations_have_one_winner() {
        let downloads = Downloads::default();
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let workers: Vec<_> = (0..8)
            .map(|i| {
                let downloads = downloads.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    downloads
                        .begin_model(format!("request {i}"), PathBuf::from("models/shared.gguf"))
                        .is_ok()
                })
            })
            .collect();
        let winners = workers
            .into_iter()
            .map(|worker| worker.join().expect("worker"))
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1);
        assert_eq!(downloads.snapshot().len(), 1);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_destination_reservations_ignore_case() {
        let downloads = Downloads::default();
        downloads
            .begin_model("First".into(), PathBuf::from("models/Model.GGUF"))
            .expect("first");
        assert!(
            downloads
                .begin_model("Second".into(), PathBuf::from("models/model.gguf"))
                .is_err()
        );
    }

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
