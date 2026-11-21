//! Auto-update — query the GitHub Releases channel, compare versions, and (in later phases) download
//! and apply a new release behind an overwrite guard.
//!
//! This module is built **incrementally** across the auto-update phases, so a few items are defined
//! before they have a caller (hence the module-wide `dead_code` allowance):
//!   - Phase 1 (here): the `UpdateMode` setting lives in `xyscript-xpl`; this module does the version
//!     parse/compare and — behind the optional `update` cargo feature — the network check.
//!   - Phase 2: the manifest-diff guard planner.
//!   - Phase 3: download + staging + SHA-256 verification (consumes `Asset`/`ReleaseInfo.notes`).
//!   - Phase 4: apply + exe swap + backup/rollback.
//!
//! The network code (ureq + serde_json) is behind the `update` feature; the pure version logic and the
//! plain-data types are always compiled so they stay unit-testable in a lean build.
#![allow(dead_code)]

/// The running build's version string (tracks `Cargo.toml`), e.g. `"0.1.14"`. Same as `$VE`.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The release channel: GitHub's "latest release" endpoint for the project repo. `/releases/latest`
/// already excludes drafts and pre-releases, so it returns exactly the newest shipped version.
pub const RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/CLDistefano/xyscript/releases/latest";

/// A downloadable asset attached to a release (one per-platform bundle, plus — from Phase 2 — the
/// `manifest.json`). `url`/`size` are consumed by the Phase 3 downloader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// What a release-channel query yields: the tag, its parsed version (if it parses), the release notes
/// (Markdown), and the attached assets. `notes`/`assets` are consumed by later phases (NOTES tab,
/// downloader).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub tag: String,
    pub version: Option<(u32, u32, u32)>,
    pub notes: String,
    pub assets: Vec<Asset>,
}

/// Parse a release tag (`"v0.1.14"`, `"0.1.14"`, `"1.2"`, `"1.2.3-rc1"`) into a `(major, minor, patch)`
/// triple. A missing minor/patch is `0`; a pre-release/build suffix (`-rc1`, `+meta`) is ignored for
/// the comparison. Returns `None` if the major component isn't numeric.
pub fn parse_version(tag: &str) -> Option<(u32, u32, u32)> {
    let trimmed = tag.trim().trim_start_matches(['v', 'V']);
    // Compare on the release core only — drop any `-prerelease` / `+build` suffix.
    let core = trimmed.split(['-', '+']).next().unwrap_or(trimmed);
    let mut it = core.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next().unwrap_or("0").parse().ok()?;
    let patch = it.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

/// Is `latest` a newer version than `current`? Plain lexicographic compare on the `(maj, min, pat)`
/// triple — correct for the project's simple `MAJOR.MINOR.PATCH` scheme.
pub fn is_newer(latest: (u32, u32, u32), current: (u32, u32, u32)) -> bool {
    latest > current
}

/// Parse the JSON body of a GitHub `/releases/latest` response into a [`ReleaseInfo`]. Tolerant of
/// missing optional fields; errors only when there's no `tag_name` (e.g. a `{ "message": "Not Found" }`
/// error object when the repo has no releases yet).
#[cfg(feature = "update")]
pub fn parse_release_json(body: &str) -> Result<ReleaseInfo, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("bad JSON from the release API: {e}"))?;
    let tag = v
        .get("tag_name")
        .and_then(|t| t.as_str())
        .ok_or_else(|| {
            let msg = v
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("response had no tag_name");
            format!("no release found ({msg})")
        })?
        .to_string();
    let notes = v.get("body").and_then(|b| b.as_str()).unwrap_or("").to_string();
    let mut assets = Vec::new();
    if let Some(arr) = v.get("assets").and_then(|a| a.as_array()) {
        for a in arr {
            let name = a.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            let url = a
                .get("browser_download_url")
                .and_then(|u| u.as_str())
                .unwrap_or("")
                .to_string();
            let size = a.get("size").and_then(|s| s.as_u64()).unwrap_or(0);
            if !name.is_empty() && !url.is_empty() {
                assets.push(Asset { name, url, size });
            }
        }
    }
    let version = parse_version(&tag);
    Ok(ReleaseInfo {
        tag,
        version,
        notes,
        assets,
    })
}

/// Query the release channel for the latest release. Behind the `update` feature (ureq, rustls — no
/// OpenSSL). A generic `User-Agent` is sent (GitHub rejects requests without one); it deliberately
/// carries **no** personal or project-identifying detail. Short timeouts so a manual check can't hang
/// the caller for long.
#[cfg(feature = "update")]
pub fn fetch_latest() -> Result<ReleaseInfo, String> {
    let resp = ureq::get(RELEASES_LATEST_URL)
        .set("User-Agent", "xyscript-updater")
        .set("Accept", "application/vnd.github+json")
        .set("X-GitHub-Api-Version", "2022-11-28")
        .timeout(std::time::Duration::from_secs(10))
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => format!("release API returned HTTP {code}"),
            other => format!("network error: {other}"),
        })?;
    let body = resp
        .into_string()
        .map_err(|e| format!("could not read the release API response: {e}"))?;
    parse_release_json(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tags_with_and_without_v_and_suffixes() {
        assert_eq!(parse_version("v0.1.14"), Some((0, 1, 14)));
        assert_eq!(parse_version("0.1.14"), Some((0, 1, 14)));
        assert_eq!(parse_version("V1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("2"), Some((2, 0, 0)));
        assert_eq!(parse_version("1.2.3-rc1"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2.3+build.5"), Some((1, 2, 3)));
        assert_eq!(parse_version("latest"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn newer_compares_by_component() {
        assert!(is_newer((0, 2, 0), (0, 1, 14))); // minor beats a bigger patch
        assert!(is_newer((0, 1, 15), (0, 1, 14)));
        assert!(is_newer((1, 0, 0), (0, 9, 9)));
        assert!(!is_newer((0, 1, 14), (0, 1, 14))); // equal is not newer
        assert!(!is_newer((0, 1, 2), (0, 1, 14))); // 2 < 14 numerically
    }

    #[cfg(feature = "update")]
    #[test]
    fn parses_a_release_payload() {
        let body = r#"{
            "tag_name": "v0.1.15",
            "body": "Fixes: a thing",
            "assets": [
                {"name": "xyscript-windows-x86_64.zip", "browser_download_url": "https://example/w.zip", "size": 1234},
                {"name": "manifest.json", "browser_download_url": "https://example/m.json", "size": 56}
            ]
        }"#;
        let info = parse_release_json(body).expect("parse");
        assert_eq!(info.tag, "v0.1.15");
        assert_eq!(info.version, Some((0, 1, 15)));
        assert!(info.notes.contains("a thing"));
        assert_eq!(info.assets.len(), 2);
        assert_eq!(info.assets[0].size, 1234);
        assert_eq!(info.assets[1].name, "manifest.json");
    }

    #[cfg(feature = "update")]
    #[test]
    fn error_object_has_no_release() {
        let body = r#"{"message": "Not Found", "documentation_url": "https://docs"}"#;
        let err = parse_release_json(body).unwrap_err();
        assert!(err.contains("Not Found"), "got: {err}");
    }
}
