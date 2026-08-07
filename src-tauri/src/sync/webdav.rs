use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::{Deserialize, Serialize};

use super::config::SyncConfig;
use super::error::{SyncError, SyncResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteBackup {
    pub filename: String,
    pub size: u64,
    pub modified_at: String,
}

/// Outcome of a MKCOL request, classified for branching in ensure_collection.
enum MkcolOutcome {
    Created,
    AlreadyExists,
    Conflict,
    Unauthorized,
    Other(u16),
}

pub struct WebDavClient {
    client: reqwest::Client,
    base_url: String,
    /// Root WebDAV URL without the `webdav_path` suffix, trailing slash stripped.
    /// Used by `ensure_collection` to create intermediate parent collections.
    root_url: String,
    auth_header: String,
}

impl WebDavClient {
    pub fn from_config(cfg: &SyncConfig) -> SyncResult<Self> {
        if cfg.webdav_url.is_empty() || cfg.webdav_username.is_empty() {
            return Err(SyncError::NotConfigured);
        }
        let base_url = join_url(&cfg.webdav_url, &cfg.webdav_path);
        let root_url = cfg.webdav_url.trim_end_matches('/').to_string();
        let token = B64.encode(format!("{}:{}", cfg.webdav_username, cfg.webdav_password));
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()?,
            base_url,
            root_url,
            auth_header: format!("Basic {}", token),
        })
    }

    pub async fn test_connection(&self) -> SyncResult<()> {
        // First probe: is the base collection present?
        let status = self.propfind_status(&self.base_url).await?;
        if status == 404 {
            // Collection missing — try to create it (recursively if needed).
            // If creation fails because the root URL itself is unreachable,
            // surface that as a connection error rather than silently no-oping.
            self.ensure_collection().await?;
            // Re-probe to confirm creation succeeded.
            let again = self.propfind_status(&self.base_url).await?;
            if again == 404 {
                return Err(SyncError::ConnectionFailed(
                    "远程目录创建后仍不可访问".into(),
                ));
            }
            if again == 401 || again == 403 {
                return Err(SyncError::Unauthorized);
            }
            if !(200..=300).contains(&again) && again != 207 {
                return Err(SyncError::ConnectionFailed(format!("HTTP {}", again)));
            }
        }
        Ok(())
    }

    /// PROPFIND depth=0 on a URL, returning the raw HTTP status code.
    /// Used internally for existence checks where we need to branch on 404.
    async fn propfind_status(&self, url: &str) -> SyncResult<u16> {
        let resp = self
            .client
            .request(reqwest::Method::from_bytes(b"PROPFIND").unwrap(), url)
            .header("Authorization", &self.auth_header)
            .header("Depth", "0")
            .send()
            .await?;
        Ok(resp.status().as_u16())
    }

    /// Ensure the base_url collection exists, creating it (and any missing
    /// parent collections along the webdav_path) via MKCOL if necessary.
    ///
    /// Idempotent: 201 (created) and 405 (already exists) are both treated as
    /// success. If an intermediate parent is missing (409), collections are
    /// created root-outward one segment at a time.
    pub async fn ensure_collection(&self) -> SyncResult<()> {
        // Try a single MKCOL on the full base_url first (common case: only the
        // leaf is missing).
        match self.mkcol(&self.base_url).await? {
            MkcolOutcome::Created | MkcolOutcome::AlreadyExists => return Ok(()),
            MkcolOutcome::Conflict => {
                // A parent collection is missing — build up from the root.
                self.ensure_collection_recursive().await
            }
            MkcolOutcome::Unauthorized => Err(SyncError::Unauthorized),
            MkcolOutcome::Other(status) => Err(SyncError::ConnectionFailed(format!(
                "MKCOL 返回 HTTP {}",
                status
            ))),
        }
    }

    /// Create each path segment from root_url outward. The root_url itself is
    /// assumed to exist (it is the user-configured WebDAV server base); if it
    /// doesn't, the whole connection is misconfigured and we surface an error.
    async fn ensure_collection_recursive(&self) -> SyncResult<()> {
        // Derive the path segments between root_url and base_url.
        // base_url = "{root_url}/{path...}/"; strip root prefix + trailing '/'.
        let suffix = self
            .base_url
            .strip_prefix(&self.root_url)
            .unwrap_or("")
            .trim_matches('/');
        if suffix.is_empty() {
            // base_url == root_url; nothing intermediate to create.
            return self.mkcol_expect_ok(&self.base_url).await;
        }
        // Build collection URLs segment by segment.
        let mut acc = self.root_url.clone();
        for segment in suffix.split('/') {
            if segment.is_empty() {
                continue;
            }
            acc.push('/');
            acc.push_str(segment);
            let url = format!("{}/", acc);
            match self.mkcol(&url).await? {
                MkcolOutcome::Created | MkcolOutcome::AlreadyExists => continue,
                MkcolOutcome::Unauthorized => return Err(SyncError::Unauthorized),
                MkcolOutcome::Conflict => {
                    return Err(SyncError::ConnectionFailed(format!(
                        "无法创建中间目录 {}（父目录缺失）",
                        url
                    )))
                }
                MkcolOutcome::Other(status) => {
                    return Err(SyncError::ConnectionFailed(format!(
                        "MKCOL {} 返回 HTTP {}",
                        url, status
                    )))
                }
            }
        }
        Ok(())
    }

    /// Issue a MKCOL request. Returns the outcome classification.
    async fn mkcol(&self, url: &str) -> SyncResult<MkcolOutcome> {
        let resp = self
            .client
            .request(reqwest::Method::from_bytes(b"MKCOL").unwrap(), url)
            .header("Authorization", &self.auth_header)
            .send()
            .await?;
        Ok(match resp.status().as_u16() {
            200 | 201 => MkcolOutcome::Created,
            405 => MkcolOutcome::AlreadyExists,
            409 => MkcolOutcome::Conflict,
            401 | 403 => MkcolOutcome::Unauthorized,
            other => MkcolOutcome::Other(other),
        })
    }

    /// MKCOL that treats Created + AlreadyExists as Ok.
    async fn mkcol_expect_ok(&self, url: &str) -> SyncResult<()> {
        match self.mkcol(url).await? {
            MkcolOutcome::Created | MkcolOutcome::AlreadyExists => Ok(()),
            MkcolOutcome::Unauthorized => Err(SyncError::Unauthorized),
            MkcolOutcome::Conflict => Err(SyncError::ConnectionFailed(format!(
                "MKCOL {} 父目录缺失",
                url
            ))),
            MkcolOutcome::Other(status) => Err(SyncError::ConnectionFailed(format!(
                "MKCOL 返回 HTTP {}",
                status
            ))),
        }
    }

    pub async fn upload(&self, filename: &str, content: &[u8]) -> SyncResult<()> {
        let url = format!("{}{}", self.base_url, filename);
        let resp = self
            .client
            .put(&url)
            .header("Authorization", &self.auth_header)
            .header("Content-Type", "application/json")
            .body(content.to_vec())
            .send()
            .await?;
        let status = resp.status();
        // 409 Conflict usually means the parent collection is missing. Try to
        // create it (and any intermediates) once, then retry the PUT a single time.
        if status.as_u16() == 409 {
            self.ensure_collection().await?;
            let retry = self
                .client
                .put(&url)
                .header("Authorization", &self.auth_header)
                .header("Content-Type", "application/json")
                .body(content.to_vec())
                .send()
                .await?;
            Self::map_dav_error(retry.status())?;
            return Ok(());
        }
        Self::map_dav_error(status)?;
        Ok(())
    }

    pub async fn list_versions(&self) -> SyncResult<Vec<RemoteBackup>> {
        let resp = self
            .client
            .request(reqwest::Method::from_bytes(b"PROPFIND").unwrap(), &self.base_url)
            .header("Authorization", &self.auth_header)
            .header("Depth", "1")
            .header("Content-Type", "application/xml; charset=utf-8")
            .body(r#"<?xml version="1.0"?><propfind xmlns="DAV:"><prop><getcontentlength/><getlastmodified/></prop></propfind>"#)
            .send()
            .await?;
        let status = resp.status();
        // 404 means the collection doesn't exist yet. Create it, then return an
        // empty list (a freshly-created collection has no backups).
        if status.as_u16() == 404 {
            self.ensure_collection().await?;
            return Ok(Vec::new());
        }
        Self::map_dav_error(status)?;
        let xml = resp.text().await?;
        parse_propfind(&xml)
    }

    pub async fn download(&self, filename: &str) -> SyncResult<Vec<u8>> {
        let url = format!("{}{}", self.base_url, filename);
        let resp = self
            .client
            .get(&url)
            .header("Authorization", &self.auth_header)
            .send()
            .await?;
        Self::map_dav_error(resp.status())?;
        Ok(resp.bytes().await?.to_vec())
    }

    pub async fn delete(&self, filename: &str) -> SyncResult<()> {
        let url = format!("{}{}", self.base_url, filename);
        let resp = self
            .client
            .delete(&url)
            .header("Authorization", &self.auth_header)
            .send()
            .await?;
        Self::map_dav_error(resp.status())?;
        Ok(())
    }

    /// Delete the oldest backups beyond `keep`, keeping the newest `keep` files.
    /// Returns the filenames that were removed.
    pub async fn prune(&self, keep: usize) -> SyncResult<Vec<String>> {
        let versions = self.list_versions().await?;
        let to_delete = select_to_prune(&versions, keep);
        for v in &to_delete {
            self.delete(v).await?;
        }
        Ok(to_delete)
    }

    fn map_dav_error(status: reqwest::StatusCode) -> SyncResult<()> {
        match status.as_u16() {
            200..=299 => Ok(()),
            401 | 403 => Err(SyncError::Unauthorized),
            404 => Err(SyncError::NotFound),
            _ => Err(SyncError::ConnectionFailed(format!("HTTP {}", status))),
        }
    }
}

fn join_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    // Trim both ends so "backups/" or "/backups/" normalize to "backups"
    let path = path.trim_matches('/');
    if path.is_empty() {
        format!("{}/", base)
    } else {
        format!("{}/{}/", base, path)
    }
}

/// Parse a WebDAV PROPFIND multistatus XML response into a list of backups.
/// Only entries whose href basename starts with "ai-proxy-backup-" are returned.
pub fn parse_propfind(xml: &str) -> SyncResult<Vec<RemoteBackup>> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut backups = Vec::new();
    let mut current_href: Option<String> = None;
    let mut current_size: Option<u64> = None;
    let mut current_modified: Option<String> = None;
    let mut in_response = false;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let local = local_name(e.name().as_ref());
                if local == "response" {
                    in_response = true;
                    current_href = None;
                    current_size = None;
                    current_modified = None;
                }
            }
            Ok(Event::Empty(e)) => {
                let local = local_name(e.name().as_ref());
                if in_response && local == "getcontentlength" {
                    current_size = None; // empty element
                }
            }
            Ok(Event::End(e)) => {
                let local = local_name(e.name().as_ref());
                if in_response {
                    if local == "getcontentlength" {
                        // value already captured in Text event below
                    } else if local == "response" {
                        if let Some(href) = current_href.take() {
                            let filename = href.rsplit('/').next().unwrap_or("").to_string();
                            if filename.starts_with("ai-proxy-backup-") {
                                backups.push(RemoteBackup {
                                    filename,
                                    size: current_size.unwrap_or(0),
                                    modified_at: current_modified.take().unwrap_or_default(),
                                });
                            }
                        }
                        in_response = false;
                    }
                }
            }
            Ok(Event::Text(e)) => {
                let text = e.unescape()?.to_string();
                if in_response {
                    // Heuristic: assign the most recent text to whichever prop is open.
                    // To be robust, track the last opened element name.
                    if text.trim().parse::<u64>().is_ok() && current_size.is_none() {
                        current_size = text.trim().parse().ok();
                    } else if text.contains("GMT") || text.contains("UTC") || text.contains(':') {
                        if current_modified.is_none() {
                            current_modified = Some(text);
                        }
                    } else if text.starts_with('/') || text.contains("ai-proxy-backup") {
                        if current_href.is_none() {
                            current_href = Some(text);
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(SyncError::Xml(e)),
            _ => {}
        }
        buf.clear();
    }

    backups.sort_by(|a, b| b.filename.cmp(&a.filename));
    Ok(backups)
}

fn local_name(name: &[u8]) -> String {
    // Strip XML namespace prefix, e.g. "D:response" or "d:response" → "response"
    let s = std::str::from_utf8(name).unwrap_or("");
    s.rsplit(':').next().unwrap_or(s).to_string()
}

/// Given the versions list (already sorted newest-first by `parse_propfind`),
/// return the filenames to delete so at most `keep` remain. `keep == 0` keeps
/// everything (pruning disabled).
pub fn select_to_prune(versions: &[RemoteBackup], keep: usize) -> Vec<String> {
    if keep == 0 || versions.len() <= keep {
        return Vec::new();
    }
    versions[keep..]
        .iter()
        .map(|v| v.filename.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_propfind_extracts_backups() {
        let xml = r#"<?xml version="1.0"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/dav/ai-proxy-backups/ai-proxy-backup-2026-07-06T10-30-00Z.json</D:href>
    <D:propstat><D:prop>
      <D:getcontentlength>45230</D:getcontentlength>
      <D:getlastmodified>Sun, 06 Jul 2026 10:30:00 GMT</D:getlastmodified>
    </D:prop></D:propstat>
  </D:response>
  <D:response>
    <D:href>/dav/ai-proxy-backups/ai-proxy-backup-2026-07-05T09-00-00Z.json</D:href>
    <D:propstat><D:prop>
      <D:getcontentlength>43100</D:getcontentlength>
      <D:getlastmodified>Sat, 05 Jul 2026 09:00:00 GMT</D:getlastmodified>
    </D:prop></D:propstat>
  </D:response>
  <D:response>
    <D:href>/dav/ai-proxy-backups/</D:href>
  </D:response>
</D:multistatus>"#;
        let backups = parse_propfind(xml).unwrap();
        assert_eq!(backups.len(), 2);
        // Sorted descending by filename
        assert_eq!(
            backups[0].filename,
            "ai-proxy-backup-2026-07-06T10-30-00Z.json"
        );
        assert_eq!(
            backups[1].filename,
            "ai-proxy-backup-2026-07-05T09-00-00Z.json"
        );
        assert_eq!(backups[0].size, 45230);
    }

    #[test]
    fn test_join_url() {
        assert_eq!(
            join_url("https://dav.example.com/dav", "backups/"),
            "https://dav.example.com/dav/backups/"
        );
        assert_eq!(
            join_url("https://dav.example.com/dav/", ""),
            "https://dav.example.com/dav/"
        );
    }

    #[test]
    fn test_select_to_prune_keeps_newest() {
        let mk = |n| RemoteBackup {
            filename: format!("ai-proxy-backup-2026-07-{n:02}T00-00-00Z.json"),
            size: 1,
            modified_at: String::new(),
        };
        let versions: Vec<RemoteBackup> = (1..=15).rev().map(mk).collect();
        assert_eq!(
            versions[0].filename,
            "ai-proxy-backup-2026-07-15T00-00-00Z.json"
        );

        let to_delete = select_to_prune(&versions, 10);
        assert_eq!(to_delete.len(), 5);
        assert_eq!(to_delete[0], "ai-proxy-backup-2026-07-05T00-00-00Z.json");
        assert_eq!(to_delete[4], "ai-proxy-backup-2026-07-01T00-00-00Z.json");
    }

    #[test]
    fn test_select_to_prune_disabled_or_within_limit() {
        let mk = |n| RemoteBackup {
            filename: format!("f{n}"),
            size: 1,
            modified_at: String::new(),
        };
        let versions: Vec<RemoteBackup> = (1..=5).rev().map(mk).collect();
        assert_eq!(select_to_prune(&versions, 10), Vec::<String>::new());
        assert_eq!(select_to_prune(&versions, 5), Vec::<String>::new());
        assert_eq!(select_to_prune(&versions, 0), Vec::<String>::new());
    }
}
