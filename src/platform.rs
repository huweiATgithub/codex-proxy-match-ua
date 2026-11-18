use gateway_plugin_sdk::call::middleware::MiddlewareHeader;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Linux,
    Windows,
    MacOS,
}

impl Platform {
    pub fn name(self) -> &'static str {
        match self {
            Self::Linux => "Linux",
            Self::Windows => "Windows",
            Self::MacOS => "macOS",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UaIssue {
    Missing,
    Duplicate,
    Malformed,
    UnknownPlatform,
}

impl UaIssue {
    fn explanation(self) -> &'static str {
        match self {
            Self::Missing => "is missing",
            Self::Duplicate => "has multiple values",
            Self::Malformed => "has no valid Codex platform tuple",
            Self::UnknownPlatform => "reports an unrecognized operating system",
        }
    }
}

type Identified = Result<Platform, UaIssue>;

pub struct UserAgent<'a> {
    pub platform: Identified,
    pub first: Option<&'a [u8]>,
    pub count: usize,
}

impl<'a> UserAgent<'a> {
    pub fn read(headers: &'a [MiddlewareHeader]) -> Self {
        let mut values = headers
            .iter()
            .filter(|h| h.name.eq_ignore_ascii_case("user-agent"));
        let first = values.next().map(|h| h.value.as_slice());
        let count = usize::from(first.is_some()) + values.count();
        let platform = match (first, count) {
            (None, _) => Err(UaIssue::Missing),
            (Some(bytes), 1) => parse(bytes),
            _ => Err(UaIssue::Duplicate),
        };
        Self {
            platform,
            first,
            count,
        }
    }
}

fn parse(bytes: &[u8]) -> Identified {
    if !bytes.iter().all(|b| (32..=126).contains(b)) {
        return Err(UaIssue::Malformed);
    }
    let ua = std::str::from_utf8(bytes).map_err(|_| UaIssue::Malformed)?;
    let (product_version, rest) = ua.split_once(" (").ok_or(UaIssue::Malformed)?;
    let (product, version) = product_version.rsplit_once('/').ok_or(UaIssue::Malformed)?;
    if product.trim().is_empty() || version.is_empty() || version.contains(' ') {
        return Err(UaIssue::Malformed);
    }
    let (tuple, suffix) = rest.split_once(')').ok_or(UaIssue::Malformed)?;
    let (system, arch) = tuple.split_once(';').ok_or(UaIssue::Malformed)?;
    // A comma followed by another product/version and environment can be a joined duplicate.
    if suffix.split(',').skip(1).any(|part| {
        part.trim_start()
            .split_once(" (")
            .is_some_and(|(p, _)| p.contains('/'))
    }) || tuple.contains('(')
        || arch.trim().is_empty()
        || arch.contains(';')
        || !(suffix.is_empty() || suffix.starts_with(' '))
    {
        return Err(UaIssue::Malformed);
    }
    let system = system.trim();
    if !system.contains(' ') {
        return Err(UaIssue::Malformed);
    }
    // The host also permits free-form OS releases, e.g. "24.04 LTS".
    for (platform, labels) in [
        (
            Platform::Linux,
            &[
                "Linux",
                "Ubuntu",
                "Fedora",
                "Debian",
                "Alpine Linux",
                "Arch Linux",
            ][..],
        ),
        (Platform::MacOS, &["Mac OS", "macOS", "Darwin"][..]),
        (Platform::Windows, &["Windows"][..]),
    ] {
        if labels.iter().any(|label| {
            system.as_bytes().get(label.len()) == Some(&b' ')
                && system[..label.len()].eq_ignore_ascii_case(label)
        }) {
            return Ok(platform);
        }
    }
    Err(UaIssue::UnknownPlatform)
}

#[derive(Clone, Copy, Debug)]
pub struct Rejection {
    pub client: Identified,
    pub effective: Identified,
}

impl Rejection {
    pub fn compare(client: &UserAgent<'_>, effective: &UserAgent<'_>) -> Result<(), Self> {
        if matches!((client.platform, effective.platform), (Ok(a), Ok(b)) if a == b) {
            Ok(())
        } else {
            Err(Self {
                client: client.platform,
                effective: effective.platform,
            })
        }
    }

    pub fn reason(self) -> &'static str {
        match (self.client, self.effective) {
            (Err(_), Err(_)) => "both_uas_unusable",
            (Err(_), _) => "client_ua_unusable",
            (_, Err(_)) => "effective_ua_unusable",
            _ => "platform_mismatch",
        }
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UA incompatible. The client User-Agent ")?;
        match self.client {
            Ok(platform) => write!(f, "reports {}", platform.name())?,
            Err(issue) => f.write_str(issue.explanation())?,
        }
        f.write_str(". The effective User-Agent for this key ")?;
        match self.effective {
            Ok(platform) => write!(f, "requires {}.", platform.name()),
            Err(issue) => write!(f, "{}.", issue.explanation()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_published_cli_and_exec_profile_matches_its_platform_row() {
        let matrix: serde_json::Value =
            serde_json::from_str(include_str!("../tests/ua-matrix-0.167.2.json")).unwrap();
        let mut count = 0;
        for (row, data) in matrix["platforms"].as_object().unwrap() {
            let expected = match row.split('-').next().unwrap() {
                "linux" => Platform::Linux,
                "windows" => Platform::Windows,
                "macos" => Platform::MacOS,
                other => panic!("unclassified fixture row {other}"),
            };
            for entries in data["profiles"].as_object().unwrap().values() {
                if entries.is_null() {
                    continue;
                }
                for ua in entries.as_object().unwrap().values() {
                    let ua = ua.as_str().unwrap();
                    assert_eq!(parse(ua.as_bytes()), Ok(expected), "{row}: {ua}");
                    count += 1;
                }
            }
        }
        assert_eq!(count, 64);
    }

    #[test]
    fn separates_the_runtime_from_the_terminal_and_client() {
        assert_eq!(
            parse(b"T/1 (Ubuntu 24.04 LTS; aarch64) WindowsTerminal"),
            Ok(Platform::Linux)
        );
        assert_eq!(
            parse(
                b"codex-tui/0.167.2 (Ubuntu 24.4.0; aarch64) WindowsTerminal (codex-tui; 0.167.2)"
            ),
            Ok(Platform::Linux)
        );
        assert_eq!(
            parse(b"codex_exec/1 (Windows 10.0; x86_64) WindowsTerminal"),
            Ok(Platform::Windows)
        );
        assert_eq!(
            parse(b"Codex Desktop/1 (Mac OS 15.7; arm64) unknown"),
            Ok(Platform::MacOS)
        );
        assert_eq!(
            parse(b"codex_exec/1 (Alpine Linux 3.22; x86_64) xterm"),
            Ok(Platform::Linux)
        );
    }

    #[test]
    fn rejects_unusable_headers_without_taking_the_platform_from_the_terminal() {
        for invalid in [
            &b"curl/8"[..],
            b"T/1 (Ubuntu 24;) WindowsTerminal",
            b"T/1 (Linux 6; x; y)",
            b"T/1 (Linux 6; x), T/1 (Windows 10; x)",
            b"T/1 (Ubuntu 24; x)\xff",
        ] {
            assert_eq!(parse(invalid), Err(UaIssue::Malformed));
        }
        assert_eq!(
            parse(b"T/1 (Other 1; x) WindowsTerminal"),
            Err(UaIssue::UnknownPlatform)
        );
        assert_eq!(UserAgent::read(&[]).platform, Err(UaIssue::Missing));
        let header = MiddlewareHeader {
            name: "User-Agent".into(),
            value: b"T/1 (Windows 10; x)".to_vec(),
        };
        assert_eq!(
            UserAgent::read(&[header.clone(), header]).platform,
            Err(UaIssue::Duplicate)
        );
    }
}
