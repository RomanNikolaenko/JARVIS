//! Vision: screen, window and photo captures that the user attaches to a
//! message for a vision-capable model.
//!
//! Captures happen only on explicit user actions. They are kept in memory
//! for a short time (never on disk) until a message uses them; the saved
//! conversation records only a note that an image was attached.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use jarvis_ai::{DataLocality, ImageData};
use jarvis_events::EventCategory;
use jarvis_vision::image::{PreparedImage, decode, decode_base64, prepare};
use jarvis_vision::{
    CaptureCapabilities, CaptureInfo, Captured, DisplayInfo, ImageSource, WindowInfo,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::JarvisCore;
use crate::chat::ResolvedModel;
use crate::config::Settings;
use crate::error::CoreError;

/// Captures kept at once; older ones are dropped.
const MAX_CAPTURES: usize = 8;
const CAPTURE_TTL: Duration = Duration::from_secs(30 * 60);
/// Images in one message.
pub const MAX_ATTACHMENTS: usize = 4;

/// What Settings and the chat need to offer capture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "vision.ts")]
pub struct VisionStatus {
    pub capabilities: CaptureCapabilities,
    pub displays: Vec<DisplayInfo>,
    /// Why displays could not be listed.
    pub display_error: Option<String>,
}

struct Entry {
    info: CaptureInfo,
    image: PreparedImage,
    created: Instant,
}

#[derive(Default)]
pub(crate) struct CaptureStore {
    entries: Mutex<VecDeque<Entry>>,
}

impl std::fmt::Debug for CaptureStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureStore")
            .field("count", &self.lock().len())
            .finish()
    }
}

impl CaptureStore {
    fn lock(&self) -> MutexGuard<'_, VecDeque<Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn insert(&self, info: CaptureInfo, image: PreparedImage) {
        let mut entries = self.lock();
        entries.retain(|e| e.created.elapsed() < CAPTURE_TTL);
        while entries.len() >= MAX_CAPTURES {
            entries.pop_front();
        }
        entries.push_back(Entry {
            info,
            image,
            created: Instant::now(),
        });
    }

    fn get(&self, id: &str) -> Option<(CaptureInfo, PreparedImage)> {
        self.lock()
            .iter()
            .find(|e| e.info.id == id && e.created.elapsed() < CAPTURE_TTL)
            .map(|e| (e.info.clone(), e.image.clone()))
    }

    pub(crate) fn remove(&self, id: &str) -> bool {
        let mut entries = self.lock();
        let before = entries.len();
        entries.retain(|e| e.info.id != id);
        entries.len() != before
    }
}

/// Images attached to a chat turn, resolved from capture ids.
pub(crate) struct Attachments {
    pub images: Vec<ImageData>,
    /// Notes saved with the message instead of the images.
    pub notes: Vec<String>,
    pub ids: Vec<String>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn source_name(source: ImageSource) -> &'static str {
    match source {
        ImageSource::Screen => "screenshot",
        ImageSource::Window => "window capture",
        ImageSource::Selection => "screen selection",
        ImageSource::File => "image file",
        ImageSource::Camera => "camera photo",
    }
}

impl JarvisCore {
    pub async fn vision_status(&self) -> VisionStatus {
        let capabilities = self.capture.capabilities();
        let (displays, display_error) = match self.capture.displays().await {
            Ok(displays) => (displays, None),
            Err(err) => (Vec::new(), Some(err.to_string())),
        };
        VisionStatus {
            capabilities,
            displays,
            display_error,
        }
    }

    pub async fn list_windows(&self) -> Result<Vec<WindowInfo>, CoreError> {
        Ok(self.capture.windows().await?)
    }

    /// Captures a display (`None`: the primary one) on the user's request.
    pub async fn capture_screen(&self, display: Option<&str>) -> Result<CaptureInfo, CoreError> {
        let captured = self.capture.capture_display(display).await?;
        self.store_capture(captured, ImageSource::Screen).await
    }

    pub async fn capture_window(&self, id: &str) -> Result<CaptureInfo, CoreError> {
        let captured = self.capture.capture_window(id).await?;
        self.store_capture(captured, ImageSource::Window).await
    }

    /// A window or area the user picks in the system dialog (Wayland).
    pub async fn capture_selection(&self) -> Result<CaptureInfo, CoreError> {
        let captured = self.capture.capture_selection().await?;
        self.store_capture(captured, ImageSource::Selection).await
    }

    /// An image file the user picked (PNG or JPEG).
    pub async fn attach_image_file(
        &self,
        name: &str,
        bytes: Vec<u8>,
    ) -> Result<CaptureInfo, CoreError> {
        let image = tokio::task::spawn_blocking(move || decode(&bytes))
            .await
            .map_err(|err| CoreError::Internal(err.to_string()))??;
        let captured = Captured {
            image,
            label: name.to_owned(),
        };
        self.store_capture(captured, ImageSource::File).await
    }

    /// A photo taken with the camera in the UI (base64 JPEG or PNG).
    pub async fn attach_photo(&self, data: String) -> Result<CaptureInfo, CoreError> {
        let image = tokio::task::spawn_blocking(move || decode_base64(&data))
            .await
            .map_err(|err| CoreError::Internal(err.to_string()))??;
        let captured = Captured {
            image,
            label: "Camera photo".to_owned(),
        };
        self.store_capture(captured, ImageSource::Camera).await
    }

    pub fn discard_capture(&self, id: &str) -> bool {
        self.captures.remove(id)
    }

    async fn store_capture(
        &self,
        captured: Captured,
        source: ImageSource,
    ) -> Result<CaptureInfo, CoreError> {
        let Captured { image, label } = captured;
        let prepared = tokio::task::spawn_blocking(move || prepare(image))
            .await
            .map_err(|err| CoreError::Internal(err.to_string()))??;
        let info = CaptureInfo {
            id: Uuid::new_v4().to_string(),
            source,
            label,
            width: prepared.width,
            height: prepared.height,
            preview: prepared.preview_data_url(),
            captured_at_ms: now_ms(),
        };
        // Visible later in the event log, like microphone use.
        self.events.info(
            EventCategory::Vision,
            format!(
                "Captured a {} ({}, {}×{})",
                source_name(source),
                info.label,
                info.width,
                info.height
            ),
        );
        self.captures.insert(info.clone(), prepared);
        Ok(info)
    }

    /// Resolves capture ids attached to a message.
    pub(crate) fn attachments(&self, ids: &[String]) -> Result<Attachments, CoreError> {
        if ids.len() > MAX_ATTACHMENTS {
            return Err(CoreError::Validation(format!(
                "A message may have at most {MAX_ATTACHMENTS} images"
            )));
        }
        let mut attachments = Attachments {
            images: Vec::new(),
            notes: Vec::new(),
            ids: Vec::new(),
        };
        for id in ids {
            let (info, image) = self.captures.get(id).ok_or_else(|| {
                CoreError::NotFound("An attached image has expired; capture it again".to_owned())
            })?;
            attachments.images.push(ImageData {
                media_type: "image/jpeg".to_owned(),
                base64: image.base64(),
            });
            attachments.notes.push(format!(
                "[Attached {}: {}, {}×{}]",
                source_name(info.source),
                info.label,
                info.width,
                info.height
            ));
            attachments.ids.push(id.clone());
        }
        Ok(attachments)
    }

    /// Checks that a model may receive images.
    pub(crate) fn check_vision_model(
        &self,
        model: &ResolvedModel,
        settings: &Settings,
    ) -> Result<(), CoreError> {
        if !model.capabilities.vision {
            return Err(CoreError::Validation(format!(
                "The model \"{}\" cannot see images. Choose a vision model or leave the model on automatic.",
                model.model
            )));
        }
        // A capture may show anything that was on the screen.
        if model.descriptor.locality == DataLocality::ThirdPartyCloud
            && !settings.privacy.share_captures_with_cloud
        {
            let message =
                "Images are not sent to cloud models unless allowed in Settings → Privacy";
            self.events.warning(
                EventCategory::Security,
                format!(
                    "Blocked sending an image to {}: {message}",
                    model.descriptor.display_name
                ),
            );
            return Err(CoreError::PermissionDenied(message.to_owned()));
        }
        Ok(())
    }
}
