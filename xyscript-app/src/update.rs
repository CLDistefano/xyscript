//! Auto-update — query the GitHub Releases channel, compare versions, and (in later phases) download
//! and apply a new release behind an overwrite guard.
//!
//! This module is built **incrementally** across the auto-update phases, so a few items are defined
//! before they have a caller (hence the module-wide `dead_code` allowance):
//!   - Phase 1: the `UpdateMode` setting lives in `xyscript-xpl`; this module does the version
//!     parse/compare and — behind the optional `update` cargo feature — the network check.
//!   - Phase 2 (here): the release [`Manifest`] and the manifest-diff overwrite guard ([`plan_update`]).
//!   - Phase 3: download + staging + SHA-256 verification (consumes `Asset`/`ReleaseInfo.notes`).
//!   - Phase 4: apply the [`UpdatePlan`] + exe swap + backup/rollback.
//!
//! The network/JSON/hashing code (ureq + serde_json + sha2) is behind the `update` feature; the pure
//! version logic, the manifest data model, and the guard planner are always compiled so they stay
//! unit-testable in a lean build.
#![allow(dead_code)]

use std::collections::BTreeMap;

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

// ---------------------------------------------------------------------------------------------------
// Phase 2 — the release manifest and the overwrite guard.
// ---------------------------------------------------------------------------------------------------

/// The three bundle files that ship a working default but are *edited in place* by users (unlike the
/// rest of the bundle, which is the product). They get the uniform hash guard like everything else, but
/// they are also the hard-protect list for the **bootstrap** update — the one hop from a pre-manifest
/// install, where there is no baseline to tell a pristine file from an edited one (see [`plan_update`]).
pub const SEED_FILES: &[&str] = &["startup.xpl", "xyscript.lib", "xyscript.ar"];

/// The suffix appended to a shipped default when the local copy was edited and must not be clobbered:
/// the new default is written as `<name>.new` for the user to diff/merge (the dpkg-conffile model).
pub const NEW_SUFFIX: &str = ".new";

/// A release manifest: every shipped file's path (relative to the install dir `$XSDIR`, forward
/// slashes) → its SHA-256 (lowercase hex). Serialized as `manifest.json` inside each release bundle;
/// after install it is the baseline the *next* update compares against to tell a pristine shipped file
/// from one the user edited. The manifest never lists itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    /// The release tag this manifest describes (e.g. `"v0.1.15"`); informational.
    pub release: String,
    /// `relative/path` → SHA-256 hex. A `BTreeMap` so serialization is deterministic.
    pub files: BTreeMap<String, String>,
}

impl Manifest {
    pub fn get(&self, path: &str) -> Option<&str> {
        self.files.get(path).map(String::as_str)
    }
}

/// What the guard decides to do with one path during an update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileAction {
    /// Not present locally — write the new file.
    Write,
    /// Local copy is pristine (matches the installed version's manifest) — overwrite with the new one.
    Replace,
    /// Local copy differs from the installed manifest (user-edited) — keep it, and write the new
    /// default beside it as `<name>.new` for the user to merge.
    KeepEditedWriteNew,
    /// The running binary — always replaced (via the self-replace dance in Phase 4), never guarded.
    ReplaceExe,
    /// Dropped by the new release and the local copy is pristine — delete it.
    Delete,
    /// Dropped by the new release but the local copy was user-edited — keep it, and report it.
    KeepRemoved,
}

/// One planned change to one path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanEntry {
    pub path: String,
    pub action: FileAction,
}

/// The full set of planned changes for an update. A file whose local copy already equals the new
/// release's version produces **no** entry (nothing to do), so the plan lists only real work.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpdatePlan {
    pub entries: Vec<PlanEntry>,
    /// True when there was no installed manifest (the bootstrap hop from a pre-manifest version), so
    /// the guard fell back to the seed-file hard-protect list. Surfaced in the update report.
    pub bootstrap: bool,
}

impl UpdatePlan {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    fn paths_with(&self, action: FileAction) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|e| e.action == action)
            .map(|e| e.path.as_str())
            .collect()
    }
    /// Paths kept because the local copy was user-edited; a `<name>.new` default is written beside each.
    pub fn kept_edited(&self) -> Vec<&str> {
        self.paths_with(FileAction::KeepEditedWriteNew)
    }
    /// Paths the release dropped but that the user had edited — kept rather than deleted.
    pub fn kept_removed(&self) -> Vec<&str> {
        self.paths_with(FileAction::KeepRemoved)
    }
    pub fn count(&self, action: FileAction) -> usize {
        self.entries.iter().filter(|e| e.action == action).count()
    }
}

/// Compute the overwrite-guard plan for updating an install from `old` (the installed version's
/// manifest; `None` for the bootstrap hop out of a pre-manifest release) to `new` (the incoming
/// release's manifest). `local_hash(path)` yields the current on-disk SHA-256 of a relative path, or
/// `None` if it isn't there. `exe_name` is the running binary's bundle file name (e.g. `xyscript.exe`),
/// which is always replaced rather than guarded. `seed_files` is the bootstrap hard-protect list
/// ([`SEED_FILES`]). This is **pure** — it reads no files and writes nothing; Phase 4 applies the plan.
pub fn plan_update(
    old: Option<&Manifest>,
    new: &Manifest,
    local_hash: impl Fn(&str) -> Option<String>,
    exe_name: &str,
    seed_files: &[&str],
) -> UpdatePlan {
    let mut entries = Vec::new();

    // Files the new release ships.
    for (path, new_hash) in &new.files {
        let local = local_hash(path);
        // Already the new version → nothing to do (covers a re-run on an up-to-date tree, and the exe
        // when it was already swapped).
        if local.as_deref() == Some(new_hash.as_str()) {
            continue;
        }
        if path == exe_name {
            entries.push(PlanEntry {
                path: path.clone(),
                action: FileAction::ReplaceExe,
            });
            continue;
        }
        match local {
            None => entries.push(PlanEntry {
                path: path.clone(),
                action: FileAction::Write,
            }),
            Some(cur) => {
                let pristine = match old {
                    // Normal: pristine iff the local copy matches the installed manifest's hash.
                    Some(m) => m.get(path) == Some(cur.as_str()),
                    // Bootstrap: no baseline — hard-protect the known seed files, replace the rest.
                    None => !seed_files.contains(&path.as_str()),
                };
                entries.push(PlanEntry {
                    path: path.clone(),
                    action: if pristine {
                        FileAction::Replace
                    } else {
                        FileAction::KeepEditedWriteNew
                    },
                });
            }
        }
    }

    // Files the new release dropped (only knowable with a baseline manifest).
    if let Some(old_m) = old {
        for path in old_m.files.keys() {
            if new.files.contains_key(path) || path == exe_name {
                continue;
            }
            match local_hash(path) {
                None => {} // already gone
                Some(cur) => {
                    let pristine = old_m.get(path) == Some(cur.as_str());
                    entries.push(PlanEntry {
                        path: path.clone(),
                        action: if pristine {
                            FileAction::Delete
                        } else {
                            FileAction::KeepRemoved
                        },
                    });
                }
            }
        }
    }

    // Stable, readable order for the report: by path.
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    UpdatePlan {
        entries,
        bootstrap: old.is_none(),
    }
}

/// Parse a `manifest.json` body into a [`Manifest`]. Behind the `update` feature.
#[cfg(feature = "update")]
pub fn parse_manifest(body: &str) -> Result<Manifest, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("bad manifest JSON: {e}"))?;
    let release = v
        .get("release")
        .and_then(|r| r.as_str())
        .unwrap_or("")
        .to_string();
    let obj = v
        .get("files")
        .and_then(|f| f.as_object())
        .ok_or("manifest has no \"files\" object")?;
    let mut files = BTreeMap::new();
    for (k, val) in obj {
        if let Some(h) = val.as_str() {
            files.insert(k.clone(), h.to_string());
        }
    }
    Ok(Manifest { release, files })
}

/// SHA-256 of `bytes` as lowercase hex. Behind the `update` feature (used to hash local files for the
/// guard and to verify downloads in Phase 3).
#[cfg(feature = "update")]
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
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

    // ---- Phase 2: the manifest guard planner --------------------------------------------------

    fn manifest(release: &str, files: &[(&str, &str)]) -> Manifest {
        Manifest {
            release: release.to_string(),
            files: files
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    /// An owning `local_hash` closure over a fixed set of on-disk hashes.
    fn local(files: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: std::collections::HashMap<String, String> = files
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |p: &str| map.get(p).cloned()
    }

    fn action_of(plan: &UpdatePlan, path: &str) -> Option<FileAction> {
        plan.entries.iter().find(|e| e.path == path).map(|e| e.action)
    }

    #[test]
    fn guard_classifies_each_file_against_the_baseline() {
        let old = manifest(
            "v1",
            &[
                ("xyscript.exe", "EXE_OLD"),
                ("lang/fi.lang", "FI_OLD"),   // user will edit this
                ("lang/de.lang", "DE_OLD"),   // stays pristine
                ("themes/x.xpl", "TH_OLD"),   // dropped by v2, pristine
                ("grammars/y.xpl", "GR_OLD"), // dropped by v2, user-edited
                ("xyscript.lib", "LIB_OLD"),  // seed, pristine
            ],
        );
        let new = manifest(
            "v2",
            &[
                ("xyscript.exe", "EXE_NEW"),
                ("lang/fi.lang", "FI_NEW"),
                ("lang/de.lang", "DE_NEW"),
                ("lang/el.lang", "EL_NEW"), // brand-new file
                ("xyscript.lib", "LIB_NEW"),
            ],
        );
        let localf = local(&[
            ("xyscript.exe", "EXE_OLD"),
            ("lang/fi.lang", "FI_USER"),  // edited → keep + .new
            ("lang/de.lang", "DE_OLD"),   // pristine → replace
            ("themes/x.xpl", "TH_OLD"),   // pristine → delete
            ("grammars/y.xpl", "GR_USER"),// edited → keep removed
            ("xyscript.lib", "LIB_OLD"),  // pristine seed → replace normally
            // lang/el.lang absent → write
        ]);
        let plan = plan_update(Some(&old), &new, localf, "xyscript.exe", SEED_FILES);
        assert!(!plan.bootstrap);
        assert_eq!(action_of(&plan, "xyscript.exe"), Some(FileAction::ReplaceExe));
        assert_eq!(action_of(&plan, "lang/fi.lang"), Some(FileAction::KeepEditedWriteNew));
        assert_eq!(action_of(&plan, "lang/de.lang"), Some(FileAction::Replace));
        assert_eq!(action_of(&plan, "lang/el.lang"), Some(FileAction::Write));
        assert_eq!(action_of(&plan, "xyscript.lib"), Some(FileAction::Replace));
        assert_eq!(action_of(&plan, "themes/x.xpl"), Some(FileAction::Delete));
        assert_eq!(action_of(&plan, "grammars/y.xpl"), Some(FileAction::KeepRemoved));
        assert_eq!(plan.kept_edited(), vec!["lang/fi.lang"]);
        assert_eq!(plan.kept_removed(), vec!["grammars/y.xpl"]);
    }

    #[test]
    fn unchanged_files_produce_no_work() {
        let old = manifest("v1", &[("a.txt", "H1"), ("xyscript.exe", "E1")]);
        let new = manifest("v2", &[("a.txt", "H1"), ("xyscript.exe", "E1")]);
        let plan = plan_update(
            Some(&old),
            &new,
            local(&[("a.txt", "H1"), ("xyscript.exe", "E1")]),
            "xyscript.exe",
            SEED_FILES,
        );
        assert!(plan.is_empty(), "an up-to-date tree needs no changes");
    }

    #[test]
    fn bootstrap_hard_protects_seed_files_and_replaces_the_rest() {
        let new = manifest(
            "v2",
            &[
                ("xyscript.exe", "E2"),
                ("startup.xpl", "S2"),  // seed, local differs → protect
                ("xyscript.lib", "L2"), // seed, local differs → protect
                ("lang/fi.lang", "F2"), // non-seed, local differs → replace (bootstrap residual risk)
                ("themes/x.xpl", "T2"), // non-seed, local absent → write
            ],
        );
        let localf = local(&[
            ("xyscript.exe", "E1"),
            ("startup.xpl", "S_USER"),
            ("xyscript.lib", "L_USER"),
            ("lang/fi.lang", "F_USER"),
        ]);
        let plan = plan_update(None, &new, localf, "xyscript.exe", SEED_FILES);
        assert!(plan.bootstrap);
        assert_eq!(action_of(&plan, "xyscript.exe"), Some(FileAction::ReplaceExe));
        assert_eq!(action_of(&plan, "startup.xpl"), Some(FileAction::KeepEditedWriteNew));
        assert_eq!(action_of(&plan, "xyscript.lib"), Some(FileAction::KeepEditedWriteNew));
        assert_eq!(action_of(&plan, "lang/fi.lang"), Some(FileAction::Replace));
        assert_eq!(action_of(&plan, "themes/x.xpl"), Some(FileAction::Write));
    }

    #[test]
    fn bootstrap_skips_a_seed_file_that_already_matches_the_release() {
        // A seed file identical to the shipped one is simply up to date — not a needless `.new`.
        let new = manifest("v2", &[("startup.xpl", "S2"), ("xyscript.exe", "E2")]);
        let plan = plan_update(
            None,
            &new,
            local(&[("startup.xpl", "S2"), ("xyscript.exe", "E1")]),
            "xyscript.exe",
            SEED_FILES,
        );
        assert_eq!(action_of(&plan, "startup.xpl"), None);
        assert_eq!(action_of(&plan, "xyscript.exe"), Some(FileAction::ReplaceExe));
    }

    #[cfg(feature = "update")]
    #[test]
    fn manifest_json_parses() {
        let body = r#"{"manifest_version":1,"release":"v0.1.15","files":{"lang/fi.lang":"aa","xyscript.exe":"bb"}}"#;
        let m = parse_manifest(body).expect("parse");
        assert_eq!(m.release, "v0.1.15");
        assert_eq!(m.get("lang/fi.lang"), Some("aa"));
        assert_eq!(m.files.len(), 2);
    }

    #[cfg(feature = "update")]
    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
