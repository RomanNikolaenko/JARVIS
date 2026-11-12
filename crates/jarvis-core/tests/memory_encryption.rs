//! Encrypting memory through the core: the recovery code, the keychain and
//! unlocking. Secrets live in memory or in a failing store, never in the
//! real keychain.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use jarvis_core::jarvis_memory::{ConversationPatch, FactInput, MemoryEncryption, SearchQuery};
use jarvis_core::{AppPaths, CoreOptions, ErrorCode, JarvisCore, MEMORY_KEY_SECRET};
use jarvis_security::{InMemorySecretStore, SecretError, SecretStore};
use jarvis_storage::{FileState, file_state};

/// A keychain that reads nothing and cannot store anything.
#[derive(Debug, Default)]
struct FailingSecretStore;

impl SecretStore for FailingSecretStore {
    fn get_secret(&self, _key: &str) -> Result<Option<String>, SecretError> {
        Ok(None)
    }

    fn set_secret(&self, _key: &str, _secret: &str) -> Result<(), SecretError> {
        Err(SecretError::Unavailable(
            "no keychain in this test".to_owned(),
        ))
    }

    fn delete_secret(&self, _key: &str) -> Result<(), SecretError> {
        Ok(())
    }
}

#[derive(Debug)]
struct GatedSecretStore {
    secrets: InMemorySecretStore,
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    proceed: Mutex<mpsc::Receiver<()>>,
}

impl SecretStore for GatedSecretStore {
    fn get_secret(&self, key: &str) -> Result<Option<String>, SecretError> {
        self.secrets.get_secret(key)
    }

    fn set_secret(&self, key: &str, secret: &str) -> Result<(), SecretError> {
        if key == MEMORY_KEY_SECRET
            && let Some(entered) = self.entered.lock().unwrap().take()
        {
            entered.send(()).unwrap();
            self.proceed
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .map_err(|err| SecretError::Storage(err.to_string()))?;
        }
        self.secrets.set_secret(key, secret)
    }

    fn delete_secret(&self, key: &str) -> Result<(), SecretError> {
        self.secrets.delete_secret(key)
    }
}

struct TestCore {
    core: Arc<JarvisCore>,
    paths: AppPaths,
    _dir: tempfile::TempDir,
}

impl TestCore {
    fn database(&self) -> std::path::PathBuf {
        self.paths.data_dir.join("memory").join("jarvis.db")
    }
}

async fn initialize(paths: &AppPaths, secrets: Arc<dyn SecretStore>) -> Arc<JarvisCore> {
    JarvisCore::initialize(
        CoreOptions::new(paths.clone(), "9.9.9")
            .with_static_providers(Vec::new())
            .with_provider_factory(None)
            .with_secrets(secrets),
    )
    .await
    .unwrap()
}

async fn start(secrets: Arc<dyn SecretStore>) -> TestCore {
    let dir = tempfile::tempdir().unwrap();
    let paths = AppPaths {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("data"),
        log_dir: dir.path().join("logs"),
    };
    let core = initialize(&paths, secrets).await;
    TestCore {
        core,
        paths,
        _dir: dir,
    }
}

fn fact(content: &str) -> FactInput {
    FactInput {
        id: None,
        content: content.to_owned(),
        project_id: None,
        pinned: false,
        source_conversation_id: None,
    }
}

fn is_code(code: &str) -> bool {
    let groups: Vec<&str> = code.split('-').collect();
    groups.len() == 8
        && groups
            .iter()
            .all(|g| g.len() == 6 && g.bytes().all(|b| b.is_ascii_digit()))
}

fn last_group(code: &str) -> String {
    code.rsplit('-').next().unwrap().to_owned()
}

/// Saves a conversation and a fact, then encrypts memory. Returns the code.
async fn encrypt(test: &TestCore) -> String {
    let conversation = test.core.create_conversation(None, false).await.unwrap();
    test.core
        .update_conversation(
            conversation.id,
            ConversationPatch {
                title: Some("Atlas launch plan".to_owned()),
                project_id: None,
                clear_project: false,
                pinned_note: None,
            },
        )
        .await
        .unwrap();
    test.core
        .save_fact(fact("The Atlas launch moved to Friday"))
        .await
        .unwrap();
    let code = test.core.prepare_memory_encryption().await.unwrap().code;
    test.core.encrypt_memory(last_group(&code)).await.unwrap();
    code
}

#[tokio::test(flavor = "multi_thread")]
async fn encrypting_needs_the_last_group_and_keeps_memory_working() {
    let secrets = Arc::new(InMemorySecretStore::default());
    let test = start(secrets.clone()).await;
    test.core.create_conversation(None, false).await.unwrap();
    test.core
        .save_fact(fact("The Atlas launch moved to Friday"))
        .await
        .unwrap();
    assert_eq!(
        test.core.memory_status().await.encryption,
        MemoryEncryption::Off
    );

    let code = test.core.prepare_memory_encryption().await.unwrap().code;
    assert!(is_code(&code), "{code}");
    let wrong = test
        .core
        .encrypt_memory("000000x".to_owned())
        .await
        .unwrap_err();
    assert_eq!(wrong.code(), ErrorCode::Validation);
    assert_eq!(file_state(&test.database()).unwrap(), FileState::Plain);

    test.core.encrypt_memory(last_group(&code)).await.unwrap();
    let status = test.core.memory_status().await;
    assert_eq!(status.encryption, MemoryEncryption::On);
    assert!(status.available);
    assert_eq!(file_state(&test.database()).unwrap(), FileState::Encrypted);
    assert_eq!(
        secrets.get_secret(MEMORY_KEY_SECRET).unwrap().as_deref(),
        Some(code.as_str())
    );
    let bytes = std::fs::read(test.database()).unwrap();
    assert!(
        !bytes.windows(5).any(|w| w == b"Atlas"),
        "no text in the clear"
    );

    // Memory keeps working on the encrypted file.
    let hits = test
        .core
        .search_memory(SearchQuery {
            text: "Atlas".to_owned(),
            kinds: Vec::new(),
            limit: 10,
        })
        .await
        .unwrap();
    assert!(!hits.is_empty());
    let export = test.paths.data_dir.join("export.json");
    test.core.export_memory(&export).await.unwrap();
    assert!(export.exists());
    test.core.clear_memory().await.unwrap();
    assert_eq!(test.core.memory_status().await.counts.facts, 0);
    assert_eq!(file_state(&test.database()).unwrap(), FileState::Encrypted);

    // A second encryption is refused, and the code never reaches the log.
    let again = test.core.prepare_memory_encryption().await.unwrap_err();
    assert_eq!(again.code(), ErrorCode::Validation);
    let events = test.core.events().recent(500);
    assert!(events.iter().any(|e| e.message == "Memory is encrypted"));
    assert!(
        events
            .iter()
            .all(|e| !e.message.contains(&last_group(&code)))
    );
    test.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_without_the_key_is_locked_and_the_code_unlocks_it() {
    let test = start(Arc::new(InMemorySecretStore::default())).await;
    let code = encrypt(&test).await;
    test.core.shutdown().await;

    // The same keychain opens it at start.
    let kept = Arc::new(InMemorySecretStore::default());
    kept.set_secret(MEMORY_KEY_SECRET, &code).unwrap();
    let core = initialize(&test.paths, kept).await;
    assert_eq!(core.memory_status().await.encryption, MemoryEncryption::On);
    assert_eq!(core.memory_status().await.counts.facts, 1);
    core.shutdown().await;

    // An empty keychain leaves memory locked until the code is entered.
    let empty = Arc::new(InMemorySecretStore::default());
    let core = initialize(&test.paths, empty.clone()).await;
    let status = core.memory_status().await;
    assert_eq!(status.encryption, MemoryEncryption::Locked);
    assert!(!status.available);

    let short = core.unlock_memory(code[..code.len() - 1].to_owned()).await;
    assert_eq!(short.unwrap_err().code(), ErrorCode::Validation);
    let mut wrong = code.clone();
    wrong.replace_range(0..1, if code.starts_with('1') { "2" } else { "1" });
    let wrong = core.unlock_memory(wrong).await;
    assert_eq!(wrong.unwrap_err().code(), ErrorCode::Validation);
    assert!(empty.get_secret(MEMORY_KEY_SECRET).unwrap().is_none());

    // Spaces instead of dashes, around and inside, still work.
    let typed = format!("  {}  ", code.replace('-', " "));
    core.unlock_memory(typed).await.unwrap();
    let status = core.memory_status().await;
    assert_eq!(status.encryption, MemoryEncryption::On);
    assert_eq!(status.counts.facts, 1);
    assert_eq!(
        empty.get_secret(MEMORY_KEY_SECRET).unwrap().as_deref(),
        Some(code.as_str()),
        "the code is stored in its dashed form"
    );
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_keychain_leaves_the_plain_database() {
    let test = start(Arc::new(FailingSecretStore)).await;
    test.core
        .save_fact(fact("The Atlas launch moved to Friday"))
        .await
        .unwrap();
    let code = test.core.prepare_memory_encryption().await.unwrap().code;
    let err = test
        .core
        .encrypt_memory(last_group(&code))
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Unavailable);

    assert_eq!(file_state(&test.database()).unwrap(), FileState::Plain);
    let copy = test
        .paths
        .data_dir
        .join("memory")
        .join("jarvis.db.encrypting");
    assert!(!copy.exists());
    let status = test.core.memory_status().await;
    assert_eq!(status.encryption, MemoryEncryption::Unsupported);
    assert_eq!(status.counts.facts, 1);
    test.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fact_saved_while_the_keychain_waits_survives_encryption() {
    let (entered, keychain_waiting) = tokio::sync::oneshot::channel();
    let (release, proceed) = mpsc::channel();
    let secrets = Arc::new(GatedSecretStore {
        secrets: InMemorySecretStore::default(),
        entered: Mutex::new(Some(entered)),
        proceed: Mutex::new(proceed),
    });
    let test = start(secrets).await;
    let code = test.core.prepare_memory_encryption().await.unwrap().code;
    let core = Arc::clone(&test.core);
    let encrypting = tokio::spawn(async move { core.encrypt_memory(last_group(&code)).await });
    tokio::time::timeout(Duration::from_secs(10), keychain_waiting)
        .await
        .unwrap()
        .unwrap();

    test.core
        .save_fact(fact("The meeting moved to Monday"))
        .await
        .unwrap();
    let concurrent_prepare = test.core.prepare_memory_encryption().await;
    release.send(()).unwrap();
    encrypting.await.unwrap().unwrap();

    let status = test.core.memory_status().await;
    assert_eq!(status.encryption, MemoryEncryption::On);
    assert_eq!(status.counts.facts, 1);
    assert_eq!(
        concurrent_prepare.unwrap_err().code(),
        ErrorCode::LimitExceeded
    );
    test.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unlock_refuses_a_plain_database() {
    let secrets = Arc::new(InMemorySecretStore::default());
    let test = start(secrets.clone()).await;
    let code = "123456-".repeat(7) + "123456";
    let err = test.core.unlock_memory(code).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Validation);
    assert!(err.to_string().contains("not encrypted"), "{err}");
    assert!(secrets.get_secret(MEMORY_KEY_SECRET).unwrap().is_none());
    test.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn encrypting_needs_a_prepared_code() {
    let test = start(Arc::new(InMemorySecretStore::default())).await;
    let err = test
        .core
        .encrypt_memory("123456".to_owned())
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Validation);
    assert_eq!(file_state(&test.database()).unwrap(), FileState::Plain);
    test.core.shutdown().await;
}
