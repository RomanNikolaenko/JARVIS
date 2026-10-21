//! Vision through the core: captures attached to chat turns, routing to a
//! vision model, and the privacy rule for cloud models.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::{StreamExt, stream};
use image::RgbaImage;
use jarvis_ai::{
    AiError, AiProvider, ChatChunk, ChatRequest, ChatStream, DataLocality, FinishReason,
    ModelCapabilities, ModelInfo, ProviderDescriptor, ProviderId, ProviderKind,
};
use jarvis_core::jarvis_vision::{
    CaptureCapabilities, CaptureSupport, Captured, DisplayInfo, ImageSource, ScreenCapture,
    VisionError, WindowInfo,
};
use jarvis_core::{
    AppPaths, ChatStreamEvent, ChatTurnRequest, CoreOptions, ErrorCode, JarvisCore, SinkClosed,
};
use tokio::sync::mpsc;
use uuid::Uuid;

/// A display and one window, drawn in plain colors.
#[derive(Debug)]
struct FakeScreen;

#[async_trait]
impl ScreenCapture for FakeScreen {
    fn capabilities(&self) -> CaptureCapabilities {
        CaptureCapabilities {
            screen: CaptureSupport::Available,
            window: CaptureSupport::Available,
            selection: CaptureSupport::Unavailable,
            notes: Vec::new(),
        }
    }

    async fn displays(&self) -> jarvis_core::jarvis_vision::Result<Vec<DisplayInfo>> {
        Ok(vec![DisplayInfo {
            id: "1".into(),
            name: "Test display".into(),
            width: 2560,
            height: 1440,
            primary: true,
        }])
    }

    async fn windows(&self) -> jarvis_core::jarvis_vision::Result<Vec<WindowInfo>> {
        Ok(vec![WindowInfo {
            id: "42".into(),
            title: "Notes".into(),
            app: Some("notes".into()),
            width: 800,
            height: 600,
        }])
    }

    async fn capture_display(
        &self,
        _id: Option<&str>,
    ) -> jarvis_core::jarvis_vision::Result<Captured> {
        Ok(Captured {
            image: RgbaImage::from_pixel(2560, 1440, image::Rgba([10, 20, 30, 255])),
            label: "Test display".into(),
        })
    }

    async fn capture_window(&self, id: &str) -> jarvis_core::jarvis_vision::Result<Captured> {
        if id != "42" {
            return Err(VisionError::NotFound("the window was closed".into()));
        }
        Ok(Captured {
            image: RgbaImage::from_pixel(800, 600, image::Rgba([200, 200, 200, 255])),
            label: "Notes".into(),
        })
    }

    async fn capture_selection(&self) -> jarvis_core::jarvis_vision::Result<Captured> {
        Err(VisionError::Unsupported("no system dialog here".into()))
    }
}

/// A provider with one model that sees images and one that does not.
#[derive(Debug)]
struct Eyes {
    descriptor: ProviderDescriptor,
    requests: Mutex<Vec<ChatRequest>>,
}

impl Eyes {
    fn new(locality: DataLocality) -> Arc<Self> {
        Arc::new(Self {
            descriptor: ProviderDescriptor {
                id: ProviderId::new("eyes"),
                display_name: "Eyes".into(),
                kind: if locality == DataLocality::ThirdPartyCloud {
                    ProviderKind::Cloud
                } else {
                    ProviderKind::LocalServer
                },
                locality,
            },
            requests: Mutex::default(),
        })
    }
}

#[async_trait]
impl AiProvider for Eyes {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    async fn health_check(&self) -> Result<(), AiError> {
        Ok(())
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
        let model = |id: &str, vision: bool| ModelInfo {
            id: id.into(),
            display_name: id.into(),
            provider_id: self.descriptor.id.clone(),
            context_window: None,
            capabilities: ModelCapabilities {
                chat: true,
                streaming: true,
                vision,
                ..ModelCapabilities::default()
            },
        };
        Ok(vec![model("text-only", false), model("vision-model", true)])
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<ChatStream, AiError> {
        self.requests.lock().unwrap().push(request);
        Ok(stream::iter([
            Ok(ChatChunk::Delta("I see a dark screen.".into())),
            Ok(ChatChunk::Done {
                finish_reason: FinishReason::Stop,
                usage: None,
            }),
        ])
        .boxed())
    }
}

struct TestCore {
    core: Arc<JarvisCore>,
    eyes: Arc<Eyes>,
    _dir: tempfile::TempDir,
}

async fn start(locality: DataLocality) -> TestCore {
    let dir = tempfile::tempdir().unwrap();
    let paths = AppPaths {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("data"),
        log_dir: dir.path().join("logs"),
    };
    let eyes = Eyes::new(locality);
    let core = JarvisCore::initialize(
        CoreOptions::new(paths, "9.9.9")
            .with_static_providers(vec![eyes.clone()])
            .with_provider_factory(None)
            .with_screen_capture(Arc::new(FakeScreen)),
    )
    .await
    .unwrap();
    TestCore {
        core,
        eyes,
        _dir: dir,
    }
}

fn turn(
    conversation_id: Uuid,
    text: &str,
    model: Option<&str>,
    attachments: Vec<String>,
) -> ChatTurnRequest {
    ChatTurnRequest {
        conversation_id,
        content: text.into(),
        provider_id: model.map(|_| ProviderId::new("eyes")),
        model: model.map(str::to_owned),
        temperature: None,
        max_output_tokens: None,
        speak: false,
        attachments,
    }
}

async fn send(
    core: &JarvisCore,
    request: ChatTurnRequest,
) -> Result<String, jarvis_core::CoreError> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    core.send_message(request, move |event| tx.send(event).map_err(|_| SinkClosed))
        .await?;
    let mut reply = String::new();
    while let Some(event) = rx.recv().await {
        match event {
            ChatStreamEvent::Delta { text } => reply.push_str(&text),
            ChatStreamEvent::Finished { .. } | ChatStreamEvent::Failed { .. } => break,
            ChatStreamEvent::Started { .. } => {}
        }
    }
    Ok(reply)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_screenshot_reaches_the_vision_model_once() {
    let test = start(DataLocality::OnDevice).await;
    let status = test.core.vision_status().await;
    assert_eq!(status.capabilities.screen, CaptureSupport::Available);
    assert_eq!(status.displays[0].name, "Test display");

    let capture = test.core.capture_screen(None).await.unwrap();
    assert_eq!(capture.source, ImageSource::Screen);
    // Downscaled for the model, with a preview for the UI.
    assert_eq!((capture.width, capture.height), (1568, 882));
    assert!(capture.preview.starts_with("data:image/jpeg;base64,"));

    let conversation = test.core.create_conversation(None, false).await.unwrap();
    let reply = send(
        &test.core,
        turn(
            conversation.id,
            "Що на екрані?",
            None,
            vec![capture.id.clone()],
        ),
    )
    .await
    .unwrap();
    assert_eq!(reply, "I see a dark screen.");

    let requests = test.eyes.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    // Automatic routing chose the model that sees images.
    assert_eq!(requests[0].model, "vision-model");
    let user = requests[0].messages.last().unwrap();
    assert_eq!(user.content, "Що на екрані?");
    assert_eq!(user.images.len(), 1);
    assert_eq!(user.images[0].media_type, "image/jpeg");
    assert!(user.images[0].base64.starts_with("/9j/"));

    // The conversation keeps a note, not the image.
    let saved = test
        .core
        .memory()
        .conversation(conversation.id)
        .await
        .unwrap();
    let note = &saved.messages[0].content;
    assert!(note.starts_with("Що на екрані?"), "{note}");
    assert!(
        note.contains("[Attached screenshot: Test display, 1568×882]"),
        "{note}"
    );

    // A capture is used once.
    let err = send(
        &test.core,
        turn(conversation.id, "Ще раз", None, vec![capture.id]),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::NotFound);
}

#[tokio::test(flavor = "multi_thread")]
async fn models_without_vision_and_cloud_models_are_refused() {
    let test = start(DataLocality::OnDevice).await;
    let window = test.core.capture_window("42").await.unwrap();
    assert_eq!(window.label, "Notes");
    assert_eq!(
        test.core.capture_window("7").await.unwrap_err().code(),
        ErrorCode::NotFound
    );
    assert_eq!(
        test.core.capture_selection().await.unwrap_err().code(),
        ErrorCode::Unsupported
    );
    let conversation = test.core.create_conversation(None, false).await.unwrap();
    let err = send(
        &test.core,
        turn(
            conversation.id,
            "Опиши",
            Some("text-only"),
            vec![window.id.clone()],
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Validation);
    assert!(err.to_string().contains("cannot see images"), "{err}");
    assert!(test.core.discard_capture(&window.id));
    assert!(!test.core.discard_capture(&window.id));

    // Cloud: even with cloud chats allowed, captures need their own switch.
    let cloud = start(DataLocality::ThirdPartyCloud).await;
    let mut settings = cloud.core.settings();
    settings.privacy.allow_cloud_providers = true;
    cloud.core.update_settings(settings.clone()).await.unwrap();
    let capture = cloud.core.capture_screen(None).await.unwrap();
    let conversation = cloud.core.create_conversation(None, false).await.unwrap();
    let err = send(
        &cloud.core,
        turn(conversation.id, "Що тут?", None, vec![capture.id.clone()]),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::PermissionDenied);
    assert!(cloud.eyes.requests.lock().unwrap().is_empty());

    settings.privacy.share_captures_with_cloud = true;
    cloud.core.update_settings(settings).await.unwrap();
    send(
        &cloud.core,
        turn(conversation.id, "Що тут?", None, vec![capture.id]),
    )
    .await
    .unwrap();
    assert_eq!(cloud.eyes.requests.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn image_files_and_photos_are_validated() {
    let test = start(DataLocality::OnDevice).await;
    let err = test
        .core
        .attach_image_file("notes.txt", b"plain text".to_vec())
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Validation);
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(RgbaImage::from_pixel(64, 32, image::Rgba([1, 2, 3, 255])))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let file = test
        .core
        .attach_image_file("diagram.png", png)
        .await
        .unwrap();
    assert_eq!(
        (file.source, file.label.as_str()),
        (ImageSource::File, "diagram.png")
    );
    assert_eq!(
        test.core
            .attach_photo("not base64!".into())
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Validation
    );
    let too_many: Vec<String> = (0..5).map(|_| file.id.clone()).collect();
    let conversation = test.core.create_conversation(None, false).await.unwrap();
    let err = send(&test.core, turn(conversation.id, "x", None, too_many))
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Validation);
}
