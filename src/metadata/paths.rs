use super::{Evidence, Field};
use crate::platform::Platform;

pub(super) fn classify(path: &str, field: Field) -> Option<Evidence> {
    let (platform, pattern) = path_pattern(path)?;
    Some(Evidence {
        field,
        pattern,
        platform,
    })
}

fn path_pattern(path: &str) -> Option<(Platform, &'static str)> {
    use Platform::{Linux, MacOS, Windows};
    let native = path.strip_prefix(r"\\?\").unwrap_or(path);
    if let [drive, b':', separator @ (b'/' | b'\\'), ..] = native.as_bytes()
        && drive.is_ascii_alphabetic()
    {
        return Some((
            Windows,
            if *separator == b'/' {
                "<drive>:/..."
            } else {
                r"<drive>:\..."
            },
        ));
    }

    let unc = path
        .strip_prefix(r"\\?\UNC\")
        .or_else(|| path.strip_prefix(r"\\"));
    if let Some(share) = unc.or_else(|| path.strip_prefix("//")) {
        let mut components = share.split(['/', '\\']);
        let server = components.next()?;
        let volume = components.next()?;
        if matches!(server, "" | "." | ".." | "?") || matches!(volume, "" | "." | "..") {
            return None;
        }
        if server.eq_ignore_ascii_case("wsl.localhost") {
            return Some((Windows, "//wsl.localhost/<distro>"));
        }
        if server.eq_ignore_ascii_case("wsl$") {
            return Some((Windows, "//wsl$/<distro>"));
        }
        return unc.map(|_| (Windows, r"\\<server>\<share>"));
    }

    let mut components = path.strip_prefix('/')?.split('/');
    let top = components.next()?;
    let next = components.next().unwrap_or_default();
    match top {
        "Users" if !matches!(next, "" | "." | "..") => Some((MacOS, "/Users/<name>")),
        "home" if !matches!(next, "" | "." | "..") => Some((Linux, "/home/<name>")),
        "root" => Some((Linux, "/root")),
        "mnt" if matches!(next.as_bytes(), [drive] if drive.is_ascii_alphabetic()) => {
            Some((Linux, "/mnt/<drive>"))
        }
        _ => None,
    }
}
