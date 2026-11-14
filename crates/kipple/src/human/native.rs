//! The words for a tool's own command in review (04 §5, §6): what runs, on what, and what
//! it can't take back. Never a file list: the tool decides what it removes when it runs.

use std::path::Path;

use kipple_core::{
    Finding, Kept, NativeArg, NativeCommand, NativeOutput, NativeSpec, Printed, ResolvedTool,
};

use super::{escaped, worktree};

/// What a native command means, said before a person confirms one.
pub(crate) const NATIVE_CANNOT_UNDO: &str =
    "A tool's own command cannot be undone, and it decides what it removes when it runs.";

/// What npm's command removes, beside its line.
const NPM_REMOVES: &str = "removes what is in the cache when it runs, nothing else";

/// What review shows for a path kipple fills in below its own data directory: one that
/// holds nothing, whatever its full name, which is kipple's and not the person's.
pub(crate) const KIPPLES_OWN: &str = "(kipple's own, empty)";

/// What review shows for the OS's null device.
const NOTHING: &str = "(none)";

/// What follows a stream cut short: it went past what kipple keeps, or couldn't be read
/// to its end.
pub(crate) const OUTPUT_CUT: &str = "  (the rest of what it printed wasn't kept)";

/// One argument as a person reads it. A path kipple fills in shows as what it is, never
/// as an empty value, so the line a person approves is never another valid command.
fn shown(arg: NativeArg) -> String {
    match arg {
        NativeArg::Fixed(text) => text.to_owned(),
        NativeArg::Data(key, _) => format!("{key}{KIPPLES_OWN}"),
        NativeArg::NullDevice(key) => format!("{key}{NOTHING}"),
    }
}

/// The file that runs, after the runtime that starts it if it has one, and its version:
/// two tools of one name are told apart here, and the digest names what this shows.
fn resolved(tool: &ResolvedTool) -> String {
    let runtime = tool.runtime.as_ref().map(|file| file.path.as_path());
    let files: Vec<String> = runtime
        .into_iter()
        .chain([tool.path.as_path()])
        .map(|file| escaped(&file.display().to_string()))
        .collect();
    format!("{} {}", files.join(" "), escaped(&tool.version))
}

/// A native item as the person confirming it sees it: the command, what it acts on, the
/// tool that runs it and its size, about `size` because nothing was enumerated. `path` is
/// the reviewed path: npm's own arguments name only its working directory. `finding`, the
/// one it was planned from when the frontend has it, names a worktree's repository.
pub(crate) fn native_line(
    spec: &NativeSpec,
    size: &str,
    path: &Path,
    finding: Option<&Finding>,
) -> String {
    let args: Vec<String> = spec.command.argv().into_iter().map(shown).collect();
    let line = format!(
        "  native  about {size}  {} {} on {}, run by {}",
        spec.tool.tool.program(),
        args.join(" "),
        escaped(&path.display().to_string()),
        resolved(&spec.tool)
    );
    match &spec.command {
        NativeCommand::NpmCacheClean { .. } => format!("{line}: {NPM_REMOVES}"),
        NativeCommand::GitWorktreeRemove { witness, .. } => {
            format!("{line}: {}", worktree::removal(witness, finding))
        }
    }
}

/// What a tool's own command on `path` printed, said once it ended: each stream that
/// printed anything, under a line naming the item and the stream, every line escaped, and
/// a stream cut short said to be. On stderr at the command line and in the result's
/// notices in the TUI; never on stdout and never stored (04 §8a).
pub(crate) fn output_lines(path: &Path, output: &NativeOutput) -> Vec<String> {
    let path = escaped(&path.display().to_string());
    let streams = [
        (
            format!("What the tool's command on {path} printed:"),
            &output.stdout,
        ),
        (
            format!("What the tool's command on {path} printed as errors and warnings:"),
            &output.stderr,
        ),
    ];
    let mut lines = Vec::new();
    for (heading, printed) in streams {
        let text = String::from_utf8_lossy(whole_characters(printed));
        let said: Vec<String> = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| format!("  | {}", escaped(line)))
            .collect();
        if said.is_empty() && printed.kept == Kept::All {
            continue;
        }
        lines.push(heading);
        lines.extend(said);
        if printed.kept == Kept::Start {
            lines.push(OUTPUT_CUT.to_owned());
        }
    }
    lines
}

/// The bytes of `printed` without a character the cut split, which would show as a
/// replacement character that the tool never printed.
fn whole_characters(printed: &Printed) -> &[u8] {
    let bytes = printed.bytes.as_slice();
    if printed.kept == Kept::All {
        return bytes;
    }
    let split = bytes
        .utf8_chunks()
        .last()
        .map_or(0, |chunk| chunk.invalid().len());
    bytes.get(..bytes.len() - split).unwrap_or(bytes)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kipple_core::{
        Allocated, Anchor, FileIdentity, Fingerprint, Measured, NativeCommand, NativeEffect,
        NativeEnd, NativeTool, ObjectId, RelPath, Stamp, SymbolicRoot, ToolFile, Totals,
        VerifiedRoot, WorktreeWitness,
    };

    use super::*;
    use crate::human::outcome::measured;
    use crate::human::receipt::totals;

    const IDENTITY: FileIdentity = FileIdentity { volume: 1, file: 2 };
    const FINGERPRINT: Fingerprint = Fingerprint {
        changed: STAMP,
        modified: STAMP,
        born: None,
        size: 1,
    };
    const STAMP: Stamp = match Stamp::new(1, 0) {
        Some(stamp) => stamp,
        None => panic!("a stamp in range"),
    };

    fn spec(command: NativeCommand, tool: NativeTool) -> NativeSpec {
        NativeSpec {
            command,
            tool: ResolvedTool {
                tool,
                path: PathBuf::from("/usr/bin/tool"),
                identity: IDENTITY,
                fingerprint: FINGERPRINT,
                version: "1.0.0".to_owned(),
                runtime: None,
            },
            scope: anchor(),
        }
    }

    fn anchor() -> Anchor {
        Anchor {
            root: VerifiedRoot {
                root: SymbolicRoot::ProjectRoots,
                path: PathBuf::from("/home/u"),
                identity: IDENTITY,
            },
            relative: RelPath::default(),
            parent: None,
        }
    }

    /// Catches a person confirming a command they can't read, or a tool they can't tell
    /// from another of its name: its arguments, the path they reviewed and the files and
    /// version that run it all show, never a path only kipple's data directory holds.
    #[test]
    fn a_native_line_says_what_runs_and_on_what() {
        let cache = NativeCommand::NpmCacheClean { cache: anchor() };
        let mut npm = spec(cache, NativeTool::Npm);
        npm.tool.path = PathBuf::from("/opt/node/lib/node_modules/npm/bin/npm-cli.js");
        npm.tool.version = "12.2.1".to_owned();
        npm.tool.runtime = Some(ToolFile {
            path: PathBuf::from("/opt/node/bin/node"),
            identity: IDENTITY,
            fingerprint: FINGERPRINT,
        });
        let line = native_line(&npm, "12 MiB", Path::new("/home/u/.npm/_cacache"), None);
        assert!(
            line.contains("npm cache clean --force") && line.contains("/home/u/.npm/_cacache"),
            "expected the command and the path: {line}"
        );
        assert!(
            line.contains("about 12 MiB"),
            "an estimate, said so: {line}"
        );
        assert!(
            line.contains(
                "/opt/node/bin/node /opt/node/lib/node_modules/npm/bin/npm-cli.js 12.2.1"
            ),
            "the pair that runs, and its version: {line}"
        );
        assert!(line.contains(NPM_REMOVES), "what it removes: {line}");
        assert!(
            !line.contains("native/npm"),
            "kipple's own paths stay out: {line}"
        );
        for key in [
            "--prefix=",
            "--userconfig=",
            "--globalconfig=",
            "--logs-dir=",
        ] {
            assert!(
                line.contains(&format!("{key}{KIPPLES_OWN}")),
                "{key} says kipple fills it, never an empty value: {line}"
            );
        }

        let worktree = NativeCommand::GitWorktreeRemove {
            checkout: anchor(),
            witness: WorktreeWitness {
                common: IDENTITY,
                admin: IDENTITY,
                gitfile: IDENTITY,
                gitdir_file: IDENTITY,
                head: ObjectId("a".repeat(40)),
                branch: None,
            },
        };
        let git = native_line(
            &spec(worktree, NativeTool::Git),
            "1 MiB",
            Path::new("/w"),
            None,
        );
        assert!(
            git.contains("worktree remove") && git.contains("(none)"),
            "expected the command and no device path: {git}"
        );
        assert!(
            git.contains("/usr/bin/tool 1.0.0"),
            "the file that runs, and its version: {git}"
        );
    }

    /// Catches what a tool freed read as kipple's own removal, and a measurement that
    /// fell short given a number (04 §8).
    #[test]
    fn a_native_result_never_reads_as_removed() {
        let mut ran = Totals::default();
        ran.native.items = 2;
        ran.native.reclaimed = Allocated(2048);
        ran.native.unmeasured = 1;

        let said = totals(&ran);

        assert!(!said.contains("Removed"), "{said}");
        assert!(said.contains("freed about"), "{said}");
        assert!(said.contains("1 not measured"), "{said}");
        let effect = NativeEffect {
            exit: NativeEnd {
                exit: Some(0),
                signal: None,
            },
            reclaimed: Measured::Unknown,
        };
        assert_eq!(measured(effect.reclaimed), "not measured");
    }

    /// Catches output cut at the runner's bound read as all a tool printed, which hides the
    /// reason a tool gives last, and a character the cut split shown as one the tool never
    /// printed: the cut stream says the rest wasn't kept, and ends on a whole character.
    #[test]
    fn a_cut_stream_says_so_and_ends_on_a_whole_character() {
        let cut = Printed {
            bytes: "warning: x\u{e9}".as_bytes()[..11].to_vec(),
            kept: Kept::Start,
        };
        let output = NativeOutput {
            stdout: Printed::all(b"done\n".to_vec()),
            stderr: cut,
        };

        let lines = output_lines(Path::new("/w"), &output);

        assert_eq!(
            lines[lines.len() - 2..],
            ["  | warning: x".to_owned(), OUTPUT_CUT.to_owned()]
        );
        assert!(
            lines[0].contains("/w") && lines[2].contains("/w"),
            "each heading names its item: {lines:?}"
        );
    }
}
