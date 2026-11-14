//! What an agent means by a path it was given, tool by tool. Decoding a value differently
//! from its tool names the wrong directory with no error, so the real store stays open, or
//! refuses a setup the tool accepts. Codex's rules are `AbsolutePathBuf`'s, Pi's are
//! `normalizePath`'s. A value is decoded exactly that way or refused, which holds. The
//! platform is a parameter so every OS's rules are tested on every OS.

use std::path::{Component, Path, PathBuf};

use kipple_core::Platform;

/// Whose rules decide what a path value means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tool {
    /// Codex.
    Codex,
    /// Pi.
    Pi,
}

impl Tool {
    /// What `text` names below the home directory, when it is written that way. Leading
    /// separators are gone from the answer: joining an absolute path would drop the home.
    pub(super) fn below_home(self, text: &str, platform: Platform) -> Option<&str> {
        let rest = text.strip_prefix('~')?;
        if rest.is_empty() {
            return Some(rest);
        }
        let windows = platform == Platform::Windows;
        match self {
            Self::Codex => rest
                .strip_prefix('/')
                .map(|rest| rest.trim_start_matches('/'))
                .or_else(|| {
                    let rest = rest.strip_prefix('\\').filter(|_| windows)?;
                    Some(rest.trim_start_matches('\\'))
                }),
            Self::Pi => {
                let separators: &[char] = if windows { &['/', '\\'] } else { &['/'] };
                let trimmed = rest.trim_start_matches(separators);
                (trimmed.len() < rest.len()).then_some(trimmed)
            }
        }
    }

    /// `text`, which isn't below the home, as the path the tool reads. One it can't decode
    /// stays as written, which is relative and so refused.
    pub(super) fn plain(self, text: &str, platform: Platform) -> PathBuf {
        let decoded = match self {
            Self::Codex => None,
            Self::Pi => shell_path(text, platform).or_else(|| file_url(text, platform)),
        };
        PathBuf::from(decoded.unwrap_or_else(|| text.to_owned()))
    }
}

/// `path` with `.` and `..` folded by text, as both tools fold them: `..` drops the name
/// before it, whether that name exists or is a link.
pub(super) fn folded(path: &Path) -> PathBuf {
    let mut folded = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                folded.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                folded.push(component.as_os_str());
            }
        }
    }
    folded
}

/// A Git Bash, MSYS, Cygwin or WSL drive path in the form Windows reads: `/c/x`,
/// `/cygdrive/c/x` and `/mnt/c/x` are `C:\x`.
fn shell_path(text: &str, platform: Platform) -> Option<String> {
    if platform != Platform::Windows || text.starts_with("//") || text.contains('\\') {
        return None;
    }
    let rest = text.strip_prefix('/')?;
    let below = ["mnt/", "cygdrive/"]
        .iter()
        .find_map(|prefix| without_prefix(rest, prefix));
    below.and_then(drive_path).or_else(|| drive_path(rest))
}

/// `text` past `prefix`, in any case.
fn without_prefix<'text>(text: &'text str, prefix: &str) -> Option<&'text str> {
    let (start, rest) = text.split_at_checked(prefix.len())?;
    start.eq_ignore_ascii_case(prefix).then_some(rest)
}

/// `c/x` as `C:\x`.
fn drive_path(text: &str) -> Option<String> {
    let (drive, rest) = text.split_once('/').unwrap_or((text, ""));
    let [letter] = drive.as_bytes() else {
        return None;
    };
    letter.is_ascii_alphabetic().then(|| {
        let letter = char::from(letter.to_ascii_uppercase());
        format!("{letter}:\\{}", rest.replace('/', "\\"))
    })
}

/// The path a `file://` URL names, as Node's `fileURLToPath` gives it, with its dot segments
/// still to fold. The URL parser Node runs first drops or rewrites what is refused here, so
/// reading those forms as text would name another directory: a backslash, a tab, a line
/// break and white space at the end. So is a path whose dot segments the parser folds
/// differently, see [`folds_differently`]. Also refused, as `fileURLToPath` refuses them:
/// another host outside Windows, an encoded separator, bad percent-encoding, and on Windows
/// a local path with no drive.
fn file_url(text: &str, platform: Platform) -> Option<String> {
    let rest = text.strip_prefix("file://")?;
    if text.contains(['\\', '\t', '\r', '\n']) || text.ends_with(|c| c <= ' ') {
        return None;
    }
    let rest = rest.split(['?', '#']).next()?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = if host.eq_ignore_ascii_case("localhost") {
        ""
    } else {
        host
    };
    let path = percent_decoded(path, platform)?;
    if folds_differently(host, &path, platform) {
        return None;
    }
    if platform != Platform::Windows {
        return host.is_empty().then(|| format!("/{path}"));
    }
    let path = path.replace('/', "\\");
    if !host.is_empty() {
        return plain_host(host).then(|| format!(r"\\{host}\{path}"));
    }
    let [letter, b':', b'\\', ..] = path.as_bytes() else {
        return None;
    };
    letter.is_ascii_alphabetic().then_some(path)
}

/// Whether the URL parser reads the decoded `path` differently from a fold by text. Its `..`
/// pops an empty segment, which a fold never sees. On every OS it keeps a drive that comes
/// first once dot segments are gone, so `..` can't pop it and `C|` becomes `C:`, which leaves
/// the path wrong outside Windows, where that is an ordinary name. With a host on Windows it
/// folds before the share is split off, and a fold stops at the share.
fn folds_differently(host: &str, path: &str, platform: Platform) -> bool {
    let dotted = path.split('/').any(|segment| matches!(segment, "." | ".."));
    if dotted && path.split('/').any(str::is_empty) {
        return true;
    }
    if platform == Platform::Windows {
        return dotted && !host.is_empty();
    }
    let first = path.split('/').next().is_some_and(names_a_drive);
    first || (dotted && path.split('/').any(names_a_drive))
}

/// Whether the URL parser reads `segment` as a drive, on any OS.
const fn names_a_drive(segment: &str) -> bool {
    matches!(segment.as_bytes(), [letter, b':' | b'|'] if letter.is_ascii_alphabetic())
}

/// Whether the URL parser keeps `host` as it is written. A drive (`C:`, `C|`), an escape or
/// a number it reads as an address would come out as something else.
fn plain_host(host: &str) -> bool {
    let name = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.';
    host.starts_with(|c: char| c.is_ascii_alphabetic()) && host.bytes().all(name)
}

/// `text` with its `%` escapes read, `None` if one is malformed, isn't UTF-8 or encodes a
/// path separator.
fn percent_decoded(text: &str, platform: Platform) -> Option<String> {
    let mut decoded = Vec::with_capacity(text.len());
    let mut bytes = text.bytes();
    while let Some(byte) = bytes.next() {
        if byte != b'%' {
            decoded.push(byte);
            continue;
        }
        let high = char::from(bytes.next()?).to_digit(16)?;
        let low = char::from(bytes.next()?).to_digit(16)?;
        let escaped = u8::try_from(high * 16 + low).ok()?;
        if escaped == b'/' || (platform == Platform::Windows && escaped == b'\\') {
            return None;
        }
        decoded.push(escaped);
    }
    String::from_utf8(decoded).ok()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kipple_core::Platform;

    use super::{Tool, folded};

    const LINUX: Platform = Platform::Linux;
    const WINDOWS: Platform = Platform::Windows;

    /// Catches a tilde form read by the wrong OS's or the wrong tool's rules: `~\x` is the
    /// home only on Windows, and each tool trims separators its own way.
    #[test]
    fn each_tool_reads_a_tilde_by_its_own_rules() {
        let rows = [
            (Tool::Codex, LINUX, "~", Some("")),
            (Tool::Codex, LINUX, "~//x", Some("x")),
            (Tool::Codex, LINUX, r"~\x", None),
            (Tool::Codex, LINUX, "~x", None),
            (Tool::Codex, WINDOWS, r"~\\x", Some("x")),
            (Tool::Codex, WINDOWS, r"~/\x", Some(r"\x")),
            (Tool::Pi, LINUX, "~", Some("")),
            (Tool::Pi, LINUX, "~//x", Some("x")),
            (Tool::Pi, LINUX, r"~\x", None),
            (Tool::Pi, LINUX, "~x", None),
            (Tool::Pi, WINDOWS, r"~\x", Some("x")),
            (Tool::Pi, WINDOWS, r"~/\x", Some("x")),
        ];

        for (tool, platform, text, expected) in rows {
            let found = tool.below_home(text, platform);
            assert_eq!(found, expected, "{tool:?} on {platform:?} for {text:?}");
        }
    }

    /// Catches Pi's Windows forms read as relative, which on `pi.agent` holds every finding,
    /// and Pi's URLs and shell paths applied to Codex, which reads neither.
    #[test]
    fn pi_decodes_urls_and_shell_paths_and_codex_doesnt() {
        let rows = [
            (Tool::Pi, LINUX, "file:///a/b%20c", "/a/b c"),
            (Tool::Pi, LINUX, "file://localhost/a?q#f", "/a"),
            (Tool::Pi, LINUX, "file://", "/"),
            (Tool::Pi, LINUX, "file://host/a", "file://host/a"),
            (Tool::Pi, LINUX, "file:///a%2Fb", "file:///a%2Fb"),
            (Tool::Pi, LINUX, "file:///a%zz", "file:///a%zz"),
            (Tool::Pi, LINUX, "file:///a%ff", "file:///a%ff"),
            (Tool::Pi, LINUX, "/c/x", "/c/x"),
            (Tool::Pi, LINUX, "file:///a/../b", "/b"),
            (Tool::Pi, LINUX, "file:///a/%2e%2e/b", "/b"),
            (Tool::Pi, LINUX, "file:///a/%2E./b", "/b"),
            (Tool::Pi, LINUX, "file:///a/%2e/b", "/a/b"),
            (Tool::Pi, LINUX, "file:///a/./b", "/a/b"),
            (Tool::Pi, LINUX, r"file:///a\b", r"file:///a\b"),
            (Tool::Pi, LINUX, "file:///a\tb", "file:///a\tb"),
            (Tool::Pi, LINUX, "file:///a\nb", "file:///a\nb"),
            (Tool::Pi, LINUX, "file:///a\rb", "file:///a\rb"),
            (Tool::Pi, LINUX, "file:///a ", "file:///a "),
            (Tool::Pi, LINUX, "file:///a?q ", "file:///a?q "),
            (Tool::Pi, LINUX, " file:///a", " file:///a"),
            (Tool::Pi, LINUX, "file:///a b", "/a b"),
            (Tool::Pi, LINUX, "file:///home/u/pi", "/home/u/pi"),
            (Tool::Pi, LINUX, "file:///a//b", "/a/b"),
            (Tool::Pi, WINDOWS, "file://C|/x", "file://C|/x"),
            (Tool::Pi, WINDOWS, "file://a%62/x", "file://a%62/x"),
            (Tool::Pi, WINDOWS, "file://1.1/x", "file://1.1/x"),
            (Tool::Pi, WINDOWS, "file:///C:/a/b", r"C:\a\b"),
            (
                Tool::Pi,
                WINDOWS,
                "file://server/share/a",
                r"\\server\share\a",
            ),
            (Tool::Pi, WINDOWS, "file:///a", "file:///a"),
            (Tool::Pi, WINDOWS, "/c/a/b", r"C:\a\b"),
            (Tool::Pi, WINDOWS, "/c", r"C:\"),
            (Tool::Pi, WINDOWS, "/mnt/d/a", r"D:\a"),
            (Tool::Pi, WINDOWS, "/CYGDRIVE/d/a", r"D:\a"),
            (Tool::Pi, WINDOWS, "/mnt/data/a", "/mnt/data/a"),
            (Tool::Pi, WINDOWS, "//server/a", "//server/a"),
            (Tool::Codex, LINUX, "file:///a", "file:///a"),
            (Tool::Codex, WINDOWS, "/c/a", "/c/a"),
        ];

        for (tool, platform, text, expected) in rows {
            let found = folded(&tool.plain(text, platform));
            let expected = PathBuf::from(expected);
            assert_eq!(found, expected, "{tool:?} on {platform:?} for {text:?}");
        }

        // These compare what was decoded, with no fold: on a Windows host a fold reads the
        // `C:` inside another OS's value as a drive, which that OS's kipple never meets.
        let kept = Tool::Pi.plain("file:///a/C:/x", LINUX);
        assert_eq!(kept, PathBuf::from("/a/C:/x"));

        let refused = [
            (LINUX, "file:///a//../b"),
            (LINUX, "file:///a/b//../../c"),
            (LINUX, "file:///home//../home/u/pi"),
            (LINUX, "file:///a//%2e%2e/b"),
            (LINUX, "file:///a//./b"),
            (LINUX, "file:///C:/../x"),
            (LINUX, "file:///c:/x/../../y"),
            (LINUX, "file:///c:/a/../../x"),
            (LINUX, "file:///C|/x"),
            (LINUX, "file:///C:/x"),
            (LINUX, "file:///C:"),
            (LINUX, "file:///a/../C|/x"),
            (LINUX, "file:///./C:/../x"),
            (LINUX, "file:///%2e/C:/../x"),
            (Platform::Macos, "file:///C:/x"),
            (WINDOWS, "file://server/a/../b"),
            (WINDOWS, "file://server/share/../x"),
            (WINDOWS, "file://server/share/%2e%2e/x"),
            (WINDOWS, "file://server/share/./x"),
            (WINDOWS, "file:///C:/a//../b"),
            (WINDOWS, "file://C:/x"),
            (WINDOWS, r"file:///C:\a"),
            (WINDOWS, "file:///C:/a%5Cb"),
        ];

        for (platform, text) in refused {
            let found = Tool::Pi.plain(text, platform);
            assert_eq!(found, PathBuf::from(text), "refused on {platform:?}");
        }
    }
}
