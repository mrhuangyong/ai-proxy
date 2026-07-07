#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("口令未设置，请先在备份设置中设置口令")]
    PassphraseNotSet,
    #[error("口令错误或备份文件已损坏")]
    DecryptionFailed,
    #[error("备份文件格式无效")]
    InvalidFormat,
    #[error("备份版本不兼容: {0}")]
    UnsupportedVersion(u32),
    #[error("本地快照失败: {0}")]
    SnapshotFailed(String),
    #[error("加密错误: {0}")]
    Crypto(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

pub type BackupResult<T> = Result<T, BackupError>;
