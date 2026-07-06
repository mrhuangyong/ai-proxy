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

pub struct WebDavClient {
    client: reqwest::Client,
    base_url: String,
    auth_header: String,
}

impl WebDavClient {
    pub fn from_config(cfg: &SyncConfig) -> SyncResult<Self> {
        if cfg.webdav_url.is_empty() || cfg.webdav_username.is_empty() {
            return Err(SyncError::NotConfigured);
        }
        let base_url = join_url(&cfg.webdav_url, &cfg.webdav_path);
        let token = B64.encode(format!("{}:{}", cfg.webdav_username, cfg.webdav_password));
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()?,
            base_url,
            auth_header: format!("Basic {}", token),
        })
    }

    pub async fn test_connection(&self) -> SyncResult<()> {
        let resp = self
            .client
            .request(
                reqwest::Method::from_bytes(b"PROPFIND").unwrap(),
                &self.base_url,
            )
            .header("Authorization", &self.auth_header)
            .header("Depth", "0")
            .send()
            .await?;
        Self::map_dav_error(resp.status())?;
        Ok(())
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
        Self::map_dav_error(resp.status())?;
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
        Self::map_dav_error(resp.status())?;
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
}
