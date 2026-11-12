//! Encryption of the memory database at rest.
//!
//! The key is a recovery code of 48 random digits (about 159 bits) in eight
//! groups of six. It is the SQLCipher passphrase, is kept in the OS keychain
//! under [`MEMORY_KEY_SECRET`] and is shown to the user once, before
//! encrypting: without the keychain entry only the code opens memory.
//!
//! The keychain is touched only when encryption needs it: at start when the
//! database is locked, and when encrypting or unlocking. A plain install
//! never reads it, so a locked login keyring never prompts at start.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use jarvis_events::{EventBus, EventCategory};
use jarvis_memory::{MemoryEncryption, MemoryEngine, MemoryError};
use jarvis_security::{SecretError, SecretStore};
use serde::Serialize;
use ts_rs::TS;

use crate::{CoreError, JarvisCore};

/// Keychain entry (service `jarvis`) that holds the recovery code.
pub const MEMORY_KEY_SECRET: &str = "memory-key";

const GROUPS: usize = 8;
const GROUP_DIGITS: usize = 6;
const CODE_DIGITS: usize = GROUPS * GROUP_DIGITS;
/// Free space needed to encrypt, as a share of the database size.
const SPACE_FACTOR: f64 = 1.1;

/// The recovery code, shown once before encrypting.
#[derive(Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub struct MemoryRecoveryCode {
    /// Eight groups of six digits joined by `-`.
    pub code: String,
}

impl std::fmt::Debug for MemoryRecoveryCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MemoryRecoveryCode(..)")
    }
}

/// Encryption state the core keeps between calls.
#[derive(Default)]
pub(crate) struct EncryptionState {
    /// The code shown by the last `prepare_memory_encryption`, until
    /// encryption succeeds or JARVIS closes.
    prepared: Mutex<Option<String>>,
    /// The last keychain call failed: encryption is unsupported here.
    keychain_failed: AtomicBool,
    /// Serializes preparing a code, encrypting and unlocking.
    running: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for EncryptionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptionState")
            .field("keychain_failed", &self.keychain_failed)
            .finish_non_exhaustive()
    }
}

impl EncryptionState {
    fn begin(&self) -> Result<tokio::sync::MutexGuard<'_, ()>, CoreError> {
        self.running.try_lock().map_err(|_| {
            CoreError::LimitExceeded("Memory encryption is already in progress".to_owned())
        })
    }

    fn prepared(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        self.prepared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn keychain_result<T>(&self, result: Result<T, SecretError>) -> Result<T, CoreError> {
        self.keychain_failed
            .store(result.is_err(), Ordering::Relaxed);
        result.map_err(|err| {
            CoreError::Memory(MemoryError::Unavailable(format!(
                "Encryption needs the system keychain, which is not available: {err}"
            )))
        })
    }

    /// What the Memory screen shows: the engine's state, or unsupported
    /// when plain memory could not use the keychain.
    pub(crate) fn status(&self, engine: MemoryEncryption) -> MemoryEncryption {
        match engine {
            MemoryEncryption::Off if self.keychain_failed.load(Ordering::Relaxed) => {
                MemoryEncryption::Unsupported
            }
            other => other,
        }
    }
}

/// Opens a locked database with the key from the keychain, at start.
pub(crate) async fn unlock_at_start(
    memory: &MemoryEngine,
    secrets: &Arc<dyn SecretStore>,
    state: &EncryptionState,
    events: &EventBus,
) {
    if memory.encryption() != MemoryEncryption::Locked {
        return;
    }
    match state.keychain_result(read_key(secrets).await) {
        Ok(Some(key)) => {
            if let Err(err) = memory.unlock(key).await {
                events.error(
                    EventCategory::Memory,
                    format!("The key in the system keychain does not open memory: {err}"),
                );
            }
        }
        Ok(None) => {
            events.warning(
                EventCategory::Memory,
                "Memory is encrypted and its key is not in the system keychain: unlock it with the recovery code",
            );
        }
        Err(err) => {
            events.error(EventCategory::Memory, format!("Memory is locked. {err}"));
        }
    }
}

async fn read_key(secrets: &Arc<dyn SecretStore>) -> Result<Option<String>, SecretError> {
    let secrets = Arc::clone(secrets);
    tokio::task::spawn_blocking(move || secrets.get_secret(MEMORY_KEY_SECRET))
        .await
        .unwrap_or_else(|err| Err(SecretError::Storage(err.to_string())))
}

async fn store_key(secrets: &Arc<dyn SecretStore>, code: &str) -> Result<(), SecretError> {
    let secrets = Arc::clone(secrets);
    let code = code.to_owned();
    tokio::task::spawn_blocking(move || secrets.set_secret(MEMORY_KEY_SECRET, &code))
        .await
        .unwrap_or_else(|err| Err(SecretError::Storage(err.to_string())))
}

/// 48 random digits in groups of six. Bytes from 250 up are dropped, so
/// every digit is equally likely.
fn new_code() -> Result<String, CoreError> {
    let mut digits = Vec::with_capacity(CODE_DIGITS);
    let mut bytes = [0u8; 64];
    while digits.len() < CODE_DIGITS {
        getrandom::fill(&mut bytes)
            .map_err(|err| CoreError::Internal(format!("no system randomness: {err}")))?;
        digits.extend(
            bytes
                .iter()
                .filter(|byte| **byte < 250)
                .map(|byte| b'0' + byte % 10),
        );
    }
    digits.truncate(CODE_DIGITS);
    Ok(grouped(&digits))
}

fn grouped(digits: &[u8]) -> String {
    digits
        .chunks(GROUP_DIGITS)
        .map(|group| String::from_utf8_lossy(group).into_owned())
        .collect::<Vec<_>>()
        .join("-")
}

/// The code as typed (spaces, dashes or neither) in its dashed form.
fn normalize(typed: &str) -> Result<String, CoreError> {
    let invalid = || {
        CoreError::Validation(format!(
            "A recovery code is {CODE_DIGITS} digits in {GROUPS} groups of {GROUP_DIGITS}"
        ))
    };
    if typed
        .chars()
        .any(|c| !(c.is_ascii_digit() || c == '-' || c.is_whitespace()))
    {
        return Err(invalid());
    }
    let digits: Vec<u8> = typed.bytes().filter(u8::is_ascii_digit).collect();
    if digits.len() != CODE_DIGITS {
        return Err(invalid());
    }
    Ok(grouped(&digits))
}

impl JarvisCore {
    /// Creates the recovery code to show before encrypting. Each call
    /// replaces the previous code.
    pub async fn prepare_memory_encryption(&self) -> Result<MemoryRecoveryCode, CoreError> {
        let _running = self.encryption.begin()?;
        if !self.config.current().memory.enabled {
            return Err(CoreError::Validation(
                "Turn on saving memory before encrypting it".to_owned(),
            ));
        }
        match self.memory.encryption() {
            MemoryEncryption::Off => {}
            MemoryEncryption::On => {
                return Err(CoreError::Validation(
                    "Memory is already encrypted".to_owned(),
                ));
            }
            MemoryEncryption::Locked => {
                return Err(CoreError::Validation(
                    "Unlock memory with its recovery code first".to_owned(),
                ));
            }
            MemoryEncryption::Unsupported => {}
        }
        if self.memory.database().is_none() {
            return Err(CoreError::Memory(MemoryError::Unavailable(
                self.memory
                    .unavailable_reason()
                    .unwrap_or_else(|| "the database is not open".to_owned()),
            )));
        }
        // A read shows whether the keychain works before the user writes
        // the code down.
        self.encryption
            .keychain_result(read_key(&self.secrets).await)?;
        let code = new_code()?;
        *self.encryption.prepared() = Some(code.clone());
        Ok(MemoryRecoveryCode { code })
    }

    /// Encrypts memory with the prepared code. `confirmation` is the code's
    /// last group, typed by the user to show it was written down.
    pub async fn encrypt_memory(&self, confirmation: String) -> Result<(), CoreError> {
        let _running = self.encryption.begin()?;
        let Some(code) = self.encryption.prepared().clone() else {
            return Err(CoreError::Validation(
                "Show the recovery code before encrypting memory".to_owned(),
            ));
        };
        if confirmation.trim() != code.rsplit('-').next().unwrap_or_default() {
            return Err(CoreError::Validation(
                "These digits are not the last group of the recovery code".to_owned(),
            ));
        }
        if self.memory_state.any_busy().await || self.agent_status().running_tasks > 0 {
            return Err(CoreError::LimitExceeded(
                "Stop the running replies and agent tasks before encrypting memory".to_owned(),
            ));
        }
        if self.memory.encryption() != MemoryEncryption::Off {
            return Err(CoreError::Validation(
                "Memory is not plain: it cannot be encrypted again".to_owned(),
            ));
        }
        let Some(database) = self.memory.database() else {
            return Err(CoreError::Memory(MemoryError::Unavailable(
                "the database is not open".to_owned(),
            )));
        };
        self.check_space_for_encryption(&database).await?;

        // Save the key before taking the database lock. A write accepted while
        // the keychain is busy will be included in the export.
        self.encryption
            .keychain_result(store_key(&self.secrets, &code).await)?;
        database
            .encrypt_in_place(&code)
            .await
            .map_err(MemoryError::from)?;
        *self.encryption.prepared() = None;
        self.events
            .info(EventCategory::Memory, "Memory is encrypted");
        Ok(())
    }

    /// Opens locked memory with its recovery code and keeps the code in the
    /// keychain for the next start.
    pub async fn unlock_memory(&self, code: String) -> Result<(), CoreError> {
        let code = normalize(&code)?;
        let _running = self.encryption.begin()?;
        self.memory.unlock(code.clone()).await?;
        match self
            .encryption
            .keychain_result(store_key(&self.secrets, &code).await)
        {
            Ok(()) => self.events.info(
                EventCategory::Memory,
                "Memory was unlocked; agent tasks use it after the next start",
            ),
            Err(err) => self.events.warning(
                EventCategory::Memory,
                format!(
                    "Memory was unlocked, but the code could not be saved, so enter it again after the next start. {err}"
                ),
            ),
        };
        Ok(())
    }

    async fn check_space_for_encryption(
        &self,
        database: &jarvis_storage::Database,
    ) -> Result<(), CoreError> {
        let Some(dir) = database.path().and_then(|path| path.parent()) else {
            return Ok(());
        };
        let size = database.size_bytes().await;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_precision_loss,
            clippy::cast_sign_loss
        )]
        let needed = (size as f64 * SPACE_FACTOR) as u64;
        match jarvis_platform::available_disk_space(dir) {
            Some(free) if free < needed => Err(CoreError::LimitExceeded(format!(
                "Encrypting memory needs {} MB of free disk space, {} MB are free",
                needed.div_ceil(1_000_000),
                free / 1_000_000
            ))),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_48_digits_in_groups_of_six() {
        let code = new_code().expect("code");
        let groups: Vec<&str> = code.split('-').collect();
        assert_eq!(groups.len(), GROUPS);
        assert!(
            groups
                .iter()
                .all(|g| g.len() == GROUP_DIGITS && g.bytes().all(|b| b.is_ascii_digit()))
        );
        assert_ne!(code, new_code().expect("another code"));
    }

    #[test]
    fn typed_codes_are_normalized() {
        let dashed = "123456-".repeat(7) + "123456";
        assert_eq!(normalize(&dashed).expect("dashed"), dashed);
        let spaced = format!(" {} ", dashed.replace('-', " "));
        assert_eq!(normalize(&spaced).expect("spaced"), dashed);
        let bare = dashed.replace('-', "");
        assert_eq!(normalize(&bare).expect("bare"), dashed);
        assert!(normalize(&bare[1..]).is_err(), "47 digits");
        assert!(normalize(&format!("{bare}1")).is_err(), "49 digits");
        assert!(normalize(&dashed.replace('1', "l")).is_err(), "letters");
    }

    #[test]
    fn the_code_is_not_debug_printed() {
        let code = MemoryRecoveryCode {
            code: "123456".to_owned(),
        };
        assert!(!format!("{code:?}").contains("123456"));
    }
}
