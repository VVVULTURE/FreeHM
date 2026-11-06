//! The parts of updating that need no network: version order, GitHub's release description,
//! checksum files and which addresses may be trusted. The app (`updater.rs`) does the rest.

use serde::Deserialize;
use std::cmp::Ordering;
use std::fmt;

/// The newest full release. GitHub leaves pre-releases and drafts out of this answer.
pub const LATEST_RELEASE_API: &str = "https://api.github.com/repos/VVVULTURE/FreeHM/releases/latest";
const PAGE_PREFIX: &str = "https://github.com/VVVULTURE/FreeHM/releases/";
const DOWNLOAD_PREFIX: &str = "https://github.com/VVVULTURE/FreeHM/releases/download/";

pub const CHECKSUMS_FILE: &str = "SHA256SUMS.txt";
const INSTALLER_SUFFIX: &str = "-windows-x64-setup.exe";
const APPIMAGE_SUFFIX: &str = "-x86_64.AppImage";

/// `X.Y.Z` with an optional `-suffix`, as used by the tags (`v0.2.0`, `v0.3.0-rc1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Option<String>,
}

impl Version {
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        let (numbers, pre) = match text.split_once('-') {
            Some((n, p)) if !p.is_empty() => (n, Some(p.to_string())),
            Some(_) => return None,
            None => (text, None),
        };
        let mut it = numbers.split('.').map(|n| n.parse::<u64>().ok());
        let (major, minor, patch) = (it.next()??, it.next()??, it.next()??);
        it.next().is_none().then_some(Version { major, minor, patch, pre })
    }
}

impl Ord for Version {
    fn cmp(&self, o: &Version) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(o.major, o.minor, o.patch)).then_with(|| match (&self.pre, &o.pre) {
            (None, None) => Ordering::Equal,
            // 0.2.0-rc1 comes before 0.2.0.
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (Some(a), Some(b)) => a.cmp(b),
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, o: &Version) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        match &self.pre {
            Some(p) => write!(f, "-{p}"),
            None => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub page_url: String,
    pub assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct RawRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(Deserialize)]
struct RawAsset {
    name: String,
    browser_download_url: String,
}

/// GitHub's description of a release.
pub fn parse_release(json: &str) -> Result<Release, String> {
    let r: RawRelease = serde_json::from_str(json).map_err(|e| format!("unexpected answer from GitHub: {e}"))?;
    if r.draft {
        return Err(format!("{} is a draft", r.tag_name));
    }
    let version = Version::parse(&r.tag_name).ok_or_else(|| format!("release tag '{}' is not a version", r.tag_name))?;
    let assets = r.assets.into_iter().map(|a| Asset { name: a.name, url: a.browser_download_url }).collect();
    Ok(Release { version, page_url: r.html_url, assets })
}

impl Release {
    fn ending(&self, suffix: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name.ends_with(suffix))
    }

    pub fn installer(&self) -> Option<&Asset> {
        self.ending(INSTALLER_SUFFIX)
    }

    pub fn appimage(&self) -> Option<&Asset> {
        self.ending(APPIMAGE_SUFFIX)
    }

    pub fn checksums(&self) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == CHECKSUMS_FILE)
    }

    /// Everything points at this project's own releases. A file or a page anywhere else is
    /// never downloaded or opened, whatever the description says.
    pub fn is_official(&self) -> bool {
        // These addresses are plain: `..` or an escape could climb out of the release folder.
        let plain = |u: &str| !u.contains("..") && !u.contains(['%', '\\', '?', '#']);
        self.page_url.starts_with(PAGE_PREFIX) && plain(&self.page_url) && self.assets.iter().all(|a| a.url.starts_with(DOWNLOAD_PREFIX) && plain(&a.url))
    }
}

/// The SHA-256 (lower-case hex) of `file` in a `sha256sum` listing.
pub fn expected_sha256(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, name) = line.trim().split_once(char::is_whitespace)?;
        let name = name.trim_start().trim_start_matches('*');
        (name == file && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hash.to_ascii_lowercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("{s}"))
    }

    #[test]
    fn versions_order_numerically() {
        assert!(v("0.2.0") > v("0.1.9"));
        assert!(v("0.10.0") > v("0.9.9"), "numbers, not text");
        assert!(v("1.0.0") > v("0.99.99"));
        assert_eq!(v("v0.2.0"), v("0.2.0"));
    }

    #[test]
    fn a_pre_release_comes_before_its_release() {
        assert!(v("0.2.0-rc1") < v("0.2.0"));
        assert!(v("0.2.0-rc1") > v("0.1.0"));
        assert!(v("0.2.0-rc2") > v("0.2.0-rc1"));
        assert_eq!(v("0.3.0-rc1").to_string(), "0.3.0-rc1");
    }

    #[test]
    fn nonsense_is_not_a_version() {
        for s in ["", "v", "1.2", "1.2.3.4", "a.b.c", "1.2.3-", "latest"] {
            assert!(Version::parse(s).is_none(), "{s}");
        }
    }

    const RELEASE: &str = r#"{
        "tag_name": "v0.3.0",
        "html_url": "https://github.com/VVVULTURE/FreeHM/releases/tag/v0.3.0",
        "draft": false, "prerelease": false, "body": "ignored",
        "assets": [
          {"name": "freehm_0.3.0-1_amd64.deb", "size": 1, "browser_download_url": "https://github.com/VVVULTURE/FreeHM/releases/download/v0.3.0/freehm_0.3.0-1_amd64.deb"},
          {"name": "FreeHM-0.3.0-windows-x64-setup.exe", "size": 1, "browser_download_url": "https://github.com/VVVULTURE/FreeHM/releases/download/v0.3.0/FreeHM-0.3.0-windows-x64-setup.exe"},
          {"name": "FreeHM-0.3.0-x86_64.AppImage", "size": 1, "browser_download_url": "https://github.com/VVVULTURE/FreeHM/releases/download/v0.3.0/FreeHM-0.3.0-x86_64.AppImage"},
          {"name": "SHA256SUMS.txt", "size": 1, "browser_download_url": "https://github.com/VVVULTURE/FreeHM/releases/download/v0.3.0/SHA256SUMS.txt"}
        ]}"#;

    #[test]
    fn reads_githubs_release_description() {
        let r = parse_release(RELEASE).unwrap();
        assert_eq!(r.version, v("0.3.0"));
        assert_eq!(r.installer().unwrap().name, "FreeHM-0.3.0-windows-x64-setup.exe");
        assert_eq!(r.appimage().unwrap().name, "FreeHM-0.3.0-x86_64.AppImage");
        assert_eq!(r.checksums().unwrap().name, "SHA256SUMS.txt");
        assert!(r.is_official());
    }

    #[test]
    fn rejects_what_is_not_a_usable_release() {
        assert!(parse_release("not json").is_err());
        assert!(parse_release(r#"{"message":"Not Found"}"#).is_err(), "GitHub's answer when there is no release");
        assert!(parse_release(&RELEASE.replace("\"draft\": false", "\"draft\": true")).is_err());
        assert!(parse_release(&RELEASE.replace("v0.3.0\",\n        \"html_url", "nightly\",\n        \"html_url")).is_err());
    }

    #[test]
    fn files_or_pages_elsewhere_are_not_official() {
        let moved = |from: &str, to: &str| parse_release(&RELEASE.replace(from, to)).unwrap();
        for sneaky in ["v0.3.0/../../../../someone/else/FreeHM-0.3.0-windows", "v0.3.0/%2e%2e/FreeHM-0.3.0-windows", "v0.3.0\\\\FreeHM-0.3.0-windows"] {
            let r = moved("v0.3.0/FreeHM-0.3.0-windows", sneaky);
            assert_ne!(r.assets, parse_release(RELEASE).unwrap().assets, "{sneaky}");
            assert!(!r.is_official(), "{sneaky}");
        }
        assert!(!moved("https://github.com/VVVULTURE/FreeHM/releases/download", "https://example.com/VVVULTURE/FreeHM/releases/download").is_official());
        assert!(!moved("https://github.com/VVVULTURE/FreeHM/releases/download", "http://github.com/VVVULTURE/FreeHM/releases/download").is_official());
        assert!(!moved("https://github.com/VVVULTURE/FreeHM/releases/tag", "https://github.com/someone/else/releases/tag").is_official());
    }

    #[test]
    fn finds_a_files_checksum() {
        let h = "A".repeat(64);
        let sums = format!("{}  other.txt\n{h} *FreeHM-setup.exe\n{}  short\n", "b".repeat(64), "c".repeat(10));
        assert_eq!(expected_sha256(&sums, "FreeHM-setup.exe"), Some("a".repeat(64)));
        assert_eq!(expected_sha256(&sums, "other.txt"), Some("b".repeat(64)));
        assert_eq!(expected_sha256(&sums, "missing.exe"), None);
        assert_eq!(expected_sha256(&sums, "short"), None, "not a SHA-256");
        assert_eq!(expected_sha256("", "x"), None);
    }
}
