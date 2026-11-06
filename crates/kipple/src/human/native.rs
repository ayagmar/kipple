//! The words for a tool's own command in review (04 §5, §6): what runs, on what, and what
//! it can't take back. Never a file list: the tool decides what it removes when it runs.

use std::path::Path;

use kipple_core::{NativeArg, NativeCommand, NativeSpec};

use super::escaped;

/// What a native command means, said before a person confirms one.
pub(crate) const NATIVE_CANNOT_UNDO: &str =
    "A tool's own command cannot be undone, and it decides what it removes when it runs.";

/// What npm's command removes, beside its line.
const NPM_REMOVES: &str = "removes what is in the cache when it runs, nothing else";

/// One argument as a person reads it. What kipple points a tool at below its own data
/// directory is kipple's, so only the key shows.
fn shown(arg: NativeArg) -> String {
    match arg {
        NativeArg::Fixed(text) | NativeArg::Data(text, _) => text.to_owned(),
        NativeArg::NullDevice(key) => format!("{key}(none)"),
    }
}

/// A native item as the person confirming it sees it: the command, what it acts on and
/// its size, about `size` because nothing was enumerated. `path` is the reviewed path:
/// npm's own arguments name only its working directory.
pub(crate) fn native_line(spec: &NativeSpec, size: &str, path: &Path) -> String {
    let args: Vec<String> = spec.command.argv().into_iter().map(shown).collect();
    let line = format!(
        "  native  about {size}  {} {} on {}",
        spec.tool.tool.program(),
        args.join(" "),
        escaped(&path.display().to_string())
    );
    match spec.command {
        NativeCommand::NpmCacheClean { .. } => format!("{line}: {NPM_REMOVES}"),
        NativeCommand::GitWorktreeRemove { .. } => line,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kipple_core::{
        Allocated, Anchor, FileIdentity, Measured, NativeCommand, NativeEffect, NativeEnd,
        NativeTool, ObjectId, RelPath, ResolvedTool, SymbolicRoot, Totals, VerifiedRoot,
        WorktreeWitness,
    };

    use super::*;
    use crate::human::outcome::measured;
    use crate::human::receipt::totals;

    const IDENTITY: FileIdentity = FileIdentity { volume: 1, file: 2 };

    fn spec(command: NativeCommand, tool: NativeTool) -> NativeSpec {
        NativeSpec {
            command,
            tool: ResolvedTool {
                tool,
                path: PathBuf::from("/usr/bin/tool"),
                identity: IDENTITY,
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

    /// Catches a person confirming a command they can't read: its tool, its arguments and
    /// the path they reviewed all show, never a path only kipple's data directory holds.
    #[test]
    fn a_native_line_says_what_runs_and_on_what() {
        let cache = NativeCommand::NpmCacheClean { cache: anchor() };
        let line = native_line(
            &spec(cache, NativeTool::Npm),
            "12 MiB",
            Path::new("/home/u/.npm/_cacache"),
        );
        assert!(
            line.contains("npm cache clean --force") && line.contains("/home/u/.npm/_cacache"),
            "expected the command and the path: {line}"
        );
        assert!(
            line.contains("about 12 MiB"),
            "an estimate, said so: {line}"
        );
        assert!(line.contains(NPM_REMOVES), "what it removes: {line}");
        assert!(
            !line.contains("native/npm"),
            "kipple's own paths stay out: {line}"
        );

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
        let git = native_line(&spec(worktree, NativeTool::Git), "1 MiB", Path::new("/w"));
        assert!(
            git.contains("worktree remove") && git.contains("(none)"),
            "expected the command and no device path: {git}"
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
}
