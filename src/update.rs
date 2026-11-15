//! GitHub release discovery and the once-per-process update check lifecycle.
use anyhow::{Context, Result, bail, ensure};
use semver::Version;
use serde_json::Value;
use std::{io::Read, time::Duration};

const RELEASE_API: &str = "https://api.github.com/repos/luoluoqixi/codex-switch/releases/latest";
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UpdateInfo {
    version: String,
    title: String,
    notes: String,
    url: String,
}

impl UpdateInfo {
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn title(&self) -> &str {
        &self.title
    }
    pub fn notes(&self) -> &str {
        &self.notes
    }
    pub fn url(&self) -> &str {
        &self.url
    }
}

#[derive(Default)]
pub(crate) struct UpdateChecker {
    startup_considered: bool,
    checking: bool,
    available: Option<UpdateInfo>,
}

impl UpdateChecker {
    pub fn begin_startup(&mut self, enabled: bool) -> bool {
        if self.startup_considered {
            return false;
        }
        self.startup_considered = true;
        enabled && self.begin()
    }

    pub fn begin(&mut self) -> bool {
        if self.checking {
            return false;
        }
        self.checking = true;
        true
    }

    pub fn is_checking(&self) -> bool {
        self.checking
    }
    pub fn available(&self) -> Option<&UpdateInfo> {
        self.available.as_ref()
    }

    pub fn finish(&mut self, result: &Result<Option<UpdateInfo>>) {
        self.checking = false;
        // A transient network failure does not hide a previously found update.
        if let Ok(available) = result {
            self.available = available.clone();
        }
    }
}

pub(crate) fn check_latest() -> Result<Option<UpdateInfo>> {
    check_url(RELEASE_API, crate::APP_VERSION)
}

fn check_url(url: &str, current: &str) -> Result<Option<UpdateInfo>> {
    let response = reqwest::blocking::Client::builder()
        .user_agent(format!(
            "codex-switch/{version}",
            version = crate::APP_VERSION
        ))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .context(crate::tr!("update.could_not_create_the_update_request"))?
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .context(crate::tr!("update.could_not_connect_to_github"))?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        bail!(crate::tr!("update.github_returned_an_error_status"; status = status));
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .context(crate::tr!("update.could_not_connect_to_github"))?;
    ensure!(
        bytes.len() as u64 <= MAX_RESPONSE_BYTES,
        crate::tr!("update.github_returned_invalid_release_information")
    );
    parse_release(&bytes, current)
}

fn parse_release(bytes: &[u8], current: &str) -> Result<Option<UpdateInfo>> {
    let value: Value = serde_json::from_slice(bytes).context(crate::tr!(
        "update.github_returned_invalid_release_information"
    ))?;
    if value["draft"].as_bool() == Some(true) || value["prerelease"].as_bool() == Some(true) {
        return Ok(None);
    }
    let raw_tag = value["tag_name"]
        .as_str()
        .context(crate::tr!("update.release_does_not_have_a_version_tag"))?
        .trim();
    let tag = raw_tag.strip_prefix(['v', 'V']).unwrap_or(raw_tag);
    let latest = Version::parse(tag)
        .with_context(|| crate::tr!("update.release_version_is_invalid"; version = tag))?;
    let current =
        Version::parse(current).context(crate::tr!("update.current_version_is_invalid"))?;
    if !latest.pre.is_empty() || !latest.cmp_precedence(&current).is_gt() {
        return Ok(None);
    }
    let notes = value["body"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    let title = value["name"]
        .as_str()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or(tag)
        .to_string();
    // Construct the download destination from the configured repository, never
    // navigate to an arbitrary URL supplied by a release response.
    let mut url = reqwest::Url::parse(&format!("{}/releases/tag/", crate::APP_REPOSITORY))?;
    url.path_segments_mut()
        .expect("repository URL is hierarchical")
        .pop_if_empty()
        .push(raw_tag);
    Ok(Some(UpdateInfo {
        version: tag.to_string(),
        title,
        notes,
        url: url.into(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{io::Write, net::TcpListener, thread};

    fn release(tag: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "tag_name": tag, "name": "Release notes", "body": "## Changes\n- Fixed a bug",
            "draft": false, "prerelease": false, "html_url": "https://example.com/untrusted",
        }))
        .unwrap()
    }

    #[test]
    fn stable_release_comparison_and_details() {
        for (tag, current, newer) in [
            ("v0.1.10", "0.1.5", true),
            ("V1.0.0", "0.9.99", true),
            ("0.1.5", "0.1.5", false),
            ("0.1.4", "0.1.5", false),
            ("0.1.5+build.2", "0.1.5+build.1", false),
            ("1.0.0-beta.1", "0.1.5", false),
            ("0.1.5", "0.1.5-beta.1", true),
        ] {
            let info = parse_release(&release(tag), current).unwrap();
            assert_eq!(info.is_some(), newer, "{tag} vs {current}");
            if let Some(info) = info {
                assert_eq!(info.title(), "Release notes");
                assert!(info.notes().contains("Fixed a bug"));
                assert_eq!(
                    info.url(),
                    format!("{}/releases/tag/{tag}", crate::APP_REPOSITORY)
                );
            }
        }
    }

    #[test]
    fn incomplete_or_unpublished_releases_are_handled() {
        for bytes in [b"invalid".as_slice(), b"{}", br#"{"tag_name":"latest"}"#] {
            assert!(parse_release(bytes, "0.1.5").is_err());
        }
        for field in ["draft", "prerelease"] {
            let mut value: Value = serde_json::from_slice(&release("1.0.0")).unwrap();
            value[field] = json!(true);
            assert!(
                parse_release(&serde_json::to_vec(&value).unwrap(), "0.1.5")
                    .unwrap()
                    .is_none()
            );
        }
        let info = parse_release(br#"{"tag_name":"v1.0.0","body":null,"name":""}"#, "0.1.5")
            .unwrap()
            .unwrap();
        assert_eq!(info.title(), "1.0.0");
        assert!(info.notes().is_empty());
    }

    #[test]
    fn startup_is_once_per_process_and_manual_checks_remain_available() {
        let mut checker = UpdateChecker::default();
        assert!(checker.begin_startup(true));
        assert!(!checker.begin());
        assert!(!checker.begin_startup(true));
        let available = parse_release(&release("1.0.0"), "0.1.5").unwrap();
        checker.finish(&Ok(available.clone()));
        assert!(!checker.is_checking());
        assert!(!checker.begin_startup(true));
        assert!(checker.begin());
        checker.finish(&Err(anyhow::anyhow!("network unavailable")));
        assert_eq!(checker.available(), available.as_ref());
        assert!(checker.begin());
        checker.finish(&Ok(None));
        assert!(checker.available().is_none());

        let mut disabled = UpdateChecker::default();
        assert!(!disabled.begin_startup(false));
        assert!(!disabled.begin_startup(true));
        assert!(disabled.begin());
    }

    #[test]
    fn http_release_success_missing_release_and_rate_limit() {
        for (status, body) in [
            ("200 OK", release("v1.0.0")),
            ("404 Not Found", b"{}".to_vec()),
            ("403 Forbidden", b"{}".to_vec()),
        ] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let address = listener.local_addr().unwrap();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
                assert!(request.contains("user-agent: codex-switch/"));
                assert!(request.contains("accept: application/vnd.github+json"));
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            });
            let result = check_url(&format!("http://{address}/releases/latest"), "0.1.5");
            server.join().unwrap();
            match status {
                "200 OK" => assert_eq!(result.unwrap().unwrap().version(), "1.0.0"),
                "404 Not Found" => assert!(result.unwrap().is_none()),
                _ => assert!(result.is_err()),
            }
        }
    }
}
