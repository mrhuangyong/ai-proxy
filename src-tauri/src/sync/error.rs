#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("同步未启用")]
    NotEnabled,
    #[error("WebDAV 凭据未配置")]
    NotConfigured,
    #[error("WebDAV 连接失败: {0}")]
    ConnectionFailed(String),
    #[error("WebDAV 认证失败 (401)")]
    Unauthorized,
    #[error("远程文件不存在")]
    NotFound,
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Xml(#[from] quick_xml::Error),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Backup(#[from] crate::backup::BackupError),
    #[error("{0}")]
    Other(String),
}

pub type SyncResult<T> = Result<T, SyncError>;
