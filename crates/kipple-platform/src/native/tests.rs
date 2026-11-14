//! The runner against real directories below a temp root and a fake tool the test writes
//! (spec M4 pinned 26–30). On Linux and macOS a fake is a script, which the kernel starts
//! through its `#!` line; kipple itself never starts a shell. On Windows nothing starts,
//! so a fake is only a file to name.
//!
//! Only Linux can start a child in a checked handle, so a test that needs the child to run
//! is Linux's, and macOS and Windows check that nothing starts there.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use kipple_core::{
    CancelToken, DeleteOp, ErrorCode, FileIdentity, Freezer as _, FsProbe as _, Marks,
    NativeCommand, NativeOp, NativeRunner as _, NativeSpec, NativeTool, ObjectId, ResolvedTool,
    ToolFile, WorktreeWitness,
};
use tempfile::TempDir;

use super::NativeCommands;
use crate::environment::Environment;
use crate::executor::ConfinedFs;
use crate::executor::faults::anchor;
use crate::fs_probe::NoFollowFs;

/// A temp root holding what a test runs on, a `bin` for its fakes, and kipple's data
/// directory, with a log the fakes write to.
struct Scene {
    temp: TempDir,
}

impl Scene {
    fn new() -> Self {
        let scene = Self {
            temp: TempDir::new().unwrap(),
        };
        for dir in ["root", "bin", "data"] {
            fs::create_dir_all(scene.temp.path().join(dir)).unwrap();
        }
        scene
    }

    fn root(&self) -> PathBuf {
        self.temp.path().join("root")
    }

    fn bin(&self, name: &str) -> PathBuf {
        self.temp.path().join("bin").join(name)
    }

    fn log(&self) -> PathBuf {
        self.temp.path().join("log")
    }

    /// What the fakes logged, by line; nothing when none ran.
    fn logged(&self) -> Vec<String> {
        fs::read_to_string(self.log())
            .map(|text| text.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    /// The runner, for a machine whose environment holds a `HOME`, a `PATH` and a
    /// secret, none of which but `HOME` an npm child may see.
    fn runner(&self) -> NativeCommands {
        let env = Environment::from_vars([
            (
                OsString::from("HOME"),
                self.temp.path().as_os_str().to_owned(),
            ),
            (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
            (OsString::from("NPM_TOKEN"), OsString::from("s3cr3t")),
        ]);
        NativeCommands::new(&env, Some(self.temp.path().join("data")))
    }
}

fn identity(path: &Path) -> FileIdentity {
    NoFollowFs.metadata(path).unwrap().identity.unwrap()
}

/// Writes the fake program at `path`, running `body`.
#[cfg(unix)]
fn fake(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt as _;

    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(windows)]
fn fake(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
}

/// A fake body that logs how it was started: its program, each argument, its working
/// directory, and each variable's name.
fn logging(scene: &Scene) -> String {
    let log = scene.log().display().to_string();
    format!(
        "printf 'program=%s\\n' \"$0\" >> '{log}'\n\
         for arg in \"$@\"; do printf 'arg=%s\\n' \"$arg\" >> '{log}'; done\n\
         printf 'cwd=%s\\n' \"$(pwd -P)\" >> '{log}'\n\
         /usr/bin/env | /usr/bin/sed -n 's/^\\([A-Za-z_][A-Za-z0-9_]*\\)=.*/var=\\1/p' >> '{log}'"
    )
}

/// npm as `<node> <npm-cli.js>`: a fake runtime running `body`, and the script it is given.
fn npm(scene: &Scene, body: &str) -> ResolvedTool {
    let node = scene.bin("node");
    let cli = scene.bin("npm-cli.js");
    fake(&node, body);
    fs::write(&cli, "// npm\n").unwrap();
    ResolvedTool {
        tool: NativeTool::Npm,
        identity: identity(&cli),
        path: cli,
        version: "11.6.2".to_owned(),
        runtime: Some(ToolFile {
            identity: identity(&node),
            path: node,
        }),
    }
}

/// A fake git running `body`.
fn git(scene: &Scene, body: &str) -> ResolvedTool {
    let path = scene.bin("git");
    fake(&path, body);
    ResolvedTool {
        tool: NativeTool::Git,
        identity: identity(&path),
        path,
        version: "2.56.0".to_owned(),
        runtime: None,
    }
}

/// The target at `relative` below the scene's root, frozen as review froze it.
fn frozen(scene: &Scene, relative: &str) -> DeleteOp {
    let target = anchor(&scene.root(), relative);
    let tree = ConfinedFs
        .freeze(&target, &CancelToken::new())
        .unwrap()
        .tree;
    let marks = Marks::of(&tree);
    DeleteOp::for_platform_tests(target, tree, marks)
}

/// npm's `_cacache` in the root, holding one entry, cleaned by `tool`.
fn npm_op(scene: &Scene, tool: ResolvedTool) -> NativeOp {
    fs::create_dir_all(scene.root().join("_cacache/index-v5")).unwrap();
    fs::write(scene.root().join("_cacache/index-v5/a"), b"a").unwrap();
    let target = frozen(scene, "_cacache");
    let spec = NativeSpec {
        command: NativeCommand::NpmCacheClean {
            cache: anchor(&scene.root(), ""),
        },
        tool,
        scope: target.target().anchor().clone(),
    };
    NativeOp::for_platform_tests(target, spec, None, None)
}

/// A linked worktree `wt` of the repository `repo`, both in the root, as git lays them
/// out: the checkout's `.git` file, and the admin directory with its `gitdir`.
fn worktree_layout(scene: &Scene) -> WorktreeWitness {
    let root = scene.root();
    let admin = root.join("repo/.git/worktrees/wt");
    fs::create_dir_all(&admin).unwrap();
    fs::create_dir_all(root.join("wt")).unwrap();
    fs::write(admin.join("gitdir"), b"../../../../wt/.git\n").unwrap();
    fs::write(root.join("wt/.git"), b"gitdir: ../repo/.git/worktrees/wt\n").unwrap();
    fs::write(root.join("wt/a"), b"a").unwrap();
    WorktreeWitness {
        common: identity(&root.join("repo/.git")),
        admin: identity(&admin),
        gitfile: identity(&root.join("wt/.git")),
        gitdir_file: identity(&admin.join("gitdir")),
        head: ObjectId("a".repeat(40)),
        branch: None,
    }
}

/// The removal of the worktree `worktree_layout` made, by `tool`, as reviewed with
/// `witness`.
fn worktree_op(scene: &Scene, tool: ResolvedTool, witness: WorktreeWitness) -> NativeOp {
    let target = frozen(scene, "wt");
    let checkout = target.target().anchor().clone();
    let spec = NativeSpec {
        command: NativeCommand::GitWorktreeRemove {
            checkout: checkout.clone(),
            witness,
        },
        tool,
        scope: checkout,
    };
    let root = scene.root();
    NativeOp::for_platform_tests(
        target,
        spec,
        Some(root.join("repo")),
        Some(root.join("repo/.git/worktrees/wt")),
    )
}

/// Catches kipple starting a shell or a shim a shell would run: a `.cmd` or `.bat` tool,
/// or runtime, never starts, on any OS, and on Linux the program that starts is the tool
/// itself with the command's arguments, never `sh -c` and a string (pinned 27).
#[test]
fn no_shell_and_cmd_shim_refused() {
    for (tool, runtime) in [("npm.cmd", None), ("npm-cli.js", Some("node.BAT"))] {
        let scene = Scene::new();
        let mut shim = npm(&scene, &logging(&scene));
        let path = scene.bin(tool);
        fs::write(&path, "@echo off\n").unwrap();
        shim.identity = identity(&path);
        shim.path = path;
        if let Some(runtime) = runtime {
            let started = scene.bin(runtime);
            fs::write(&started, "@echo off\n").unwrap();
            shim.runtime = Some(ToolFile {
                identity: identity(&started),
                path: started,
            });
        }
        let op = npm_op(&scene, shim);

        let refused = scene.runner().run(&op).unwrap_err();

        assert_eq!(refused.code, ErrorCode::ToolChanged, "{tool} {runtime:?}");
        assert_eq!(scene.logged(), Vec::<String>::new(), "nothing started");
    }
}

/// Catches a native command started where this OS can't hold its working directory by
/// handle: it would run from a path an ancestor swap could rebind (pinned 27).
#[cfg(not(target_os = "linux"))]
#[test]
fn nothing_starts_where_no_handle_can_be_named() {
    let ops: [fn(&Scene) -> NativeOp; 2] = [
        |scene| npm_op(scene, npm(scene, &logging(scene))),
        |scene| {
            let witness = worktree_layout(scene);
            worktree_op(scene, git(scene, &logging(scene)), witness)
        },
    ];
    for (command, made) in ops.into_iter().enumerate() {
        let scene = Scene::new();
        let op = made(&scene);

        let refused = scene.runner().run(&op).unwrap_err();

        assert_eq!(refused.code, ErrorCode::Unsupported, "command {command}");
        assert_eq!(scene.logged(), Vec::<String>::new(), "command {command}");
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::time::Duration;

    use kipple_core::{NativeStatus, Postcondition};

    use super::*;
    use crate::executor::faults::fail_syncs_after;
    use crate::runner::OUTPUT_LIMIT;

    /// Puts another file with the same bytes at `path`, as an upgrade or an attacker would.
    #[expect(
        clippy::disallowed_methods,
        reason = "a test swaps a fake tool below its temp root"
    )]
    fn swap(path: &Path) {
        use std::os::unix::fs::PermissionsExt as _;

        let bytes = fs::read(path).unwrap();
        fs::remove_file(path).unwrap();
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// What the fakes logged under `key`.
    fn values(scene: &Scene, key: &str) -> Vec<String> {
        let prefix = format!("{key}=");
        scene
            .logged()
            .iter()
            .filter_map(|line| line.strip_prefix(&prefix).map(str::to_owned))
            .collect()
    }

    /// The variable names a child saw, without the ones the fake's own shell sets.
    fn names(scene: &Scene) -> Vec<String> {
        let mut names: Vec<String> = values(scene, "var")
            .into_iter()
            .filter(|name| !["PWD", "OLDPWD", "SHLVL", "_"].contains(&name.as_str()))
            .collect();
        names.sort();
        names
    }

    /// Catches a child seeing a variable outside its closed list, such as a token or a
    /// `PATH` that picks what it runs next: npm gets `HOME` alone, beside the runner's
    /// `LC_ALL` (pinned 27).
    #[test]
    fn environment_is_the_closed_list() {
        let scene = Scene::new();
        let op = npm_op(&scene, npm(&scene, &logging(&scene)));
        scene.runner().run(&op).unwrap();
        assert_eq!(names(&scene), ["HOME", "LC_ALL"]);
    }

    /// Catches git seeing a variable outside its isolation, such as a person's
    /// `GIT_DIR` or `PATH`: git gets its isolation and the runner's `LC_ALL` (pinned 27).
    #[test]
    fn gits_environment_is_its_isolation() {
        let scene = Scene::new();
        let witness = worktree_layout(&scene);
        let op = worktree_op(&scene, git(&scene, &logging(&scene)), witness);
        scene.runner().run(&op).unwrap();
        assert_eq!(
            names(&scene),
            [
                "GIT_ALLOW_PROTOCOL",
                "GIT_CONFIG_GLOBAL",
                "GIT_CONFIG_NOSYSTEM",
                "GIT_NO_LAZY_FETCH",
                "GIT_OPTIONAL_LOCKS",
                "GIT_PAGER",
                "LC_ALL",
                "XDG_CONFIG_HOME",
            ]
        );
    }

    /// Catches the command started from a path, or anywhere but its target's checked
    /// directory: npm in its cache root, git in the checkout, with the command's own
    /// arguments after the script npm's runtime is given (pinned 27).
    #[test]
    fn child_starts_in_the_checked_directory() {
        let scene = Scene::new();
        let tool = npm(&scene, &logging(&scene));
        let script = tool.path.display().to_string();
        let op = npm_op(&scene, tool);
        scene.runner().run(&op).unwrap();
        let cwd = values(&scene, "cwd");
        assert_eq!(cwd.len(), 1);
        assert_eq!(identity(Path::new(&cwd[0])), identity(&scene.root()));
        let args = values(&scene, "arg");
        assert_eq!(args[0], script, "the runtime is given the script");
        assert_eq!(
            args[1..5],
            ["cache", "clean", "--force", "--cache=/proc/self/cwd"]
        );
    }

    /// Catches git started from a path or given `-C`: it starts in the checkout it
    /// removes and is told to remove `.` (pinned 27).
    #[test]
    fn git_starts_in_the_checkout_it_removes() {
        let scene = Scene::new();
        let witness = worktree_layout(&scene);
        let op = worktree_op(&scene, git(&scene, &logging(&scene)), witness);
        scene.runner().run(&op).unwrap();
        let cwd = values(&scene, "cwd");
        assert_eq!(
            identity(Path::new(&cwd[0])),
            identity(&scene.root().join("wt"))
        );
        let args = values(&scene, "arg");
        assert_eq!(args[args.len() - 3..], ["worktree", "remove", "."], "no -C");
    }

    /// Catches npm's clean read as done while `_cacache` is still in its root, and the
    /// configuration boundary left for a person's npmrc to fill: done when the child
    /// removed it through its own working directory, and the boundary is empty files
    /// (pinned 32, 33).
    #[test]
    fn npm_is_done_when_cacache_is_gone_from_the_roots_handle() {
        let scene = Scene::new();
        let op = npm_op(&scene, npm(&scene, "/bin/rm -rf /proc/self/cwd/_cacache"));

        let exit = scene.runner().run(&op).unwrap();

        assert_eq!(exit.postcondition, Postcondition::Met);
        assert_eq!(exit.synced, Ok(()));
        assert!(
            matches!(exit.status, NativeStatus::Ended(end) if end.succeeded()),
            "{exit:?}"
        );
        let boundary = scene.temp.path().join("data/native/npm");
        for file in ["userconfig", "globalconfig"] {
            assert_eq!(fs::read(boundary.join(file)).unwrap(), b"");
        }
    }

    /// Catches npm's word taken for the cache: a clean that exits 0 and leaves
    /// `_cacache` is unmet (pinned 32).
    #[test]
    fn npm_that_left_cacache_is_unmet() {
        let scene = Scene::new();
        let op = npm_op(&scene, npm(&scene, "exit 0"));
        let exit = scene.runner().run(&op).unwrap();
        assert_eq!(exit.postcondition, Postcondition::Unmet, "still there");
    }

    /// Catches a person's npmrc reaching the command through kipple's boundary: a config
    /// file there that holds anything is a failed start (pinned 33).
    #[test]
    fn a_boundary_file_that_holds_anything_is_a_failed_start() {
        let scene = Scene::new();
        let op = npm_op(&scene, npm(&scene, &logging(&scene)));
        let boundary = scene.temp.path().join("data/native/npm");
        fs::create_dir_all(&boundary).unwrap();
        fs::write(boundary.join("userconfig"), b"cache=/\n").unwrap();

        assert!(scene.runner().run(&op).is_err(), "never started");
        assert_eq!(scene.logged(), Vec::<String>::new());
    }

    /// Catches a tool swapped after the plan starting anyway: the file at its path,
    /// or its runtime's, is another now, and nothing starts (pinned 29).
    #[test]
    fn a_tool_swapped_after_the_plan_never_starts() {
        for swap_runtime in [false, true] {
            let scene = Scene::new();
            let tool = npm(&scene, &logging(&scene));
            let swapped = if swap_runtime {
                tool.runtime.as_ref().unwrap().path.clone()
            } else {
                tool.path.clone()
            };
            let op = npm_op(&scene, tool);
            swap(&swapped);

            let refused = scene.runner().run(&op).unwrap_err();

            assert_eq!(refused.code, ErrorCode::ToolChanged);
            assert_eq!(scene.logged(), Vec::<String>::new(), "nothing started");
        }
    }

    /// Catches a deadline that kills more than kipple's own child, or a killed command
    /// read as one that ended by itself: the child is killed, what it started runs on, and
    /// the run says it missed its deadline, which core records as indeterminate
    /// (pinned 28).
    #[test]
    fn deadline_kills_own_child_and_is_indeterminate() {
        let scene = Scene::new();
        let pid = scene.temp.path().join("pid");
        let body = format!(
            "/bin/sleep 5 & printf '%s' $! > '{}'\nexec /bin/sleep 60",
            pid.display()
        );
        let op = npm_op(&scene, npm(&scene, &body));
        let runner = scene.runner().with_deadline(Duration::from_millis(500));

        let exit = runner.run(&op).unwrap();

        assert!(
            matches!(exit.status, NativeStatus::TimedOut(end) if end.signal == Some(libc::SIGKILL)),
            "{exit:?}"
        );
        let started = fs::read_to_string(&pid).unwrap();
        assert!(
            Path::new("/proc").join(started.trim()).exists(),
            "what the child started still runs"
        );
        assert_eq!(exit.postcondition, Postcondition::Unmet);
    }

    /// Catches output past the bound growing kipple's memory or blocking the child on a
    /// full pipe: each stream keeps its first `OUTPUT_LIMIT` bytes and the run ends.
    #[test]
    fn output_past_the_bound_keeps_its_start_and_never_blocks() {
        let scene = Scene::new();
        let body = "/usr/bin/head -c 1000000 /dev/zero\n/usr/bin/head -c 1000000 /dev/zero >&2";
        let op = npm_op(&scene, npm(&scene, body));

        let exit = scene.runner().run(&op).unwrap();

        let limit = usize::try_from(OUTPUT_LIMIT).unwrap();
        assert_eq!(exit.output.stdout.len(), limit);
        assert_eq!(exit.output.stderr.len(), limit);
        assert!(
            matches!(exit.status, NativeStatus::Ended(end) if end.succeeded()),
            "{exit:?}"
        );
    }

    /// Catches the last worktree of a repository never done: git removes `worktrees/`
    /// with its last entry, so that handle is skipped and `<common>`, which lost the
    /// entry, is synced, and a sync that fails reaches core (pinned 30).
    #[test]
    fn gone_worktrees_dir_is_skipped_and_common_still_syncs() {
        for (passing, synced) in [(2, true), (1, false), (0, false)] {
            let scene = Scene::new();
            let witness = worktree_layout(&scene);
            let root = scene.root();
            let body = format!(
                "/bin/rm -rf '{}' '{}'",
                root.join("wt").display(),
                root.join("repo/.git/worktrees").display()
            );
            let op = worktree_op(&scene, git(&scene, &body), witness);
            let _failing = fail_syncs_after(passing);

            let exit = scene.runner().run(&op).unwrap();

            assert_eq!(exit.postcondition, Postcondition::Met, "{passing}");
            assert_eq!(exit.synced.is_ok(), synced, "{passing}: {exit:?}");
            if let Err(error) = &exit.synced {
                assert_eq!(error.code, ErrorCode::Unsynced);
            }
        }
    }

    /// Catches git started on a checkout whose repository isn't the one reviewed: the
    /// checkout's `.git`, `<common>`, the admin directory or its `gitdir` off the witness
    /// is a failed start, and nothing runs (pinned 29).
    #[test]
    fn final_check_refuses_a_common_or_admin_off_the_witness() {
        let elsewhere = FileIdentity { volume: 0, file: 1 };
        let off: [fn(&mut WorktreeWitness, FileIdentity); 4] = [
            |witness, other| witness.common = other,
            |witness, other| witness.admin = other,
            |witness, other| witness.gitfile = other,
            |witness, other| witness.gitdir_file = other,
        ];
        for (field, change) in off.into_iter().enumerate() {
            let scene = Scene::new();
            let mut witness = worktree_layout(&scene);
            change(&mut witness, elsewhere);
            let op = worktree_op(&scene, git(&scene, &logging(&scene)), witness);

            let refused = scene.runner().run(&op).unwrap_err();

            assert_eq!(refused.code, ErrorCode::TargetChanged, "field {field}");
            assert_eq!(scene.logged(), Vec::<String>::new(), "field {field}");
            assert!(
                scene.root().join("wt/a").exists(),
                "field {field}: the checkout stays"
            );
        }
    }
}
