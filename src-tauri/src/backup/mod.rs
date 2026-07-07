pub mod bundle;
pub mod crypto;
pub mod error;
pub mod export;
pub mod import;
pub mod sensitive;

pub use error::{BackupError, BackupResult};

use std::sync::OnceLock;
use tokio::sync::Mutex;

/// Process-wide mutex serializing backup export/import operations so a
/// restore-in-progress cannot be observed mid-flight by another request.
static BACKUP_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub fn backup_lock() -> &'static Mutex<()> {
    BACKUP_LOCK.get_or_init(|| Mutex::new(()))
}
