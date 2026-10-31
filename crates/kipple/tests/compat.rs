//! Documents an earlier release read are read the same way today
//! (`docs/08-engineering-standards.md` §8): every config file, theme, rule pack, stored
//! run and plan file in the baselines under `tests/compat/<format>/<version>/` loads, and
//! where kipple can show what it read, shows every line of the document's `.expected`
//! file. Built only with the
//! `test-roots` feature: every root, kipple's config directory included, points into a
//! temp dir.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser as _;
use kipple::theme::Themes;
use kipple::{Cli, Io};
use kipple_adapters::builtin;
use kipple_adapters::pack::{self, Catalog, Origin};
#[cfg(unix)]
use kipple_core::format::decode_plan_file;
#[cfg(unix)]
use kipple_core::{
    ApplyError, CancelToken, Depth, ErrorCode, LockState, Locked, RunId, RunRead, RunState,
    RunStore, StoreRefusal, history, read_run,
};
use kipple_platform::Environment;
#[cfg(unix)]
use kipple_platform::OsNames;

const BASELINES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/compat");

/// Each version's baseline directory of `format`.
#[expect(
    clippy::unwrap_used,
    reason = "a baseline that can't be read fails the test"
)]
fn versions(format: &str) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(Path::new(BASELINES).join(format))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    found.sort();
    found
}

/// The `.toml` documents in the baseline directory `version`, at least one.
#[expect(
    clippy::unwrap_used,
    reason = "a baseline that can't be read fails the test"
)]
fn documents(version: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(version)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    found.sort();
    assert!(!found.is_empty(), "no documents in {}", version.display());
    found
}

/// An environment whose roots, kipple's config directory among them, are all in `temp`.
#[expect(
    clippy::unwrap_used,
    reason = "a host that can't be written fails the test"
)]
fn host(temp: &Path) -> Environment {
    let roots = temp.join("roots.txt");
    let mut lines = String::new();
    for name in ["home", "xdg.config", "known.roaming-app-data"] {
        writeln!(lines, "{name} = {}", temp.join(name).display()).unwrap();
    }
    fs::write(&roots, lines).unwrap();
    Environment::from_vars([(OsString::from("KIPPLE_TEST_ROOTS"), roots.into_os_string())])
}

/// Runs `kipple <args>` and returns its exit code, stdout and stderr.
#[expect(clippy::unwrap_used, reason = "output that isn't UTF-8 fails the test")]
fn kipple(env: &Environment, args: &[&str]) -> (ExitCode, String, String) {
    let cli = Cli::parse_from(std::iter::once("kipple").chain(args.iter().copied()));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = kipple::run(&cli, env, Io::redirected(&mut out, &mut err));
    let text = |bytes| String::from_utf8(bytes).unwrap();
    (code, text(out), text(err))
}

/// Fails unless `shown` holds every line of `document`'s `.expected` file.
#[expect(
    clippy::unwrap_used,
    reason = "a baseline that can't be read fails the test"
)]
fn assert_shows(document: &Path, shown: &str) {
    let expected = fs::read_to_string(document.with_extension("expected")).unwrap();
    let lines: Vec<&str> = shown.lines().map(str::trim_end).collect();
    let missing: Vec<&str> = expected
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty() && !lines.contains(line))
        .collect();
    assert!(
        missing.is_empty(),
        "{} no longer reads as it did. If only the wording of `show` changed, update its \
         .expected file. Missing:\n{}\n\nShown:\n{shown}",
        document.display(),
        missing.join("\n"),
    );
}

// Catches a released user's config file refused after an upgrade (a key renamed or
// dropped, a value or rule ID no longer accepted) or read with another meaning.
#[test]
fn every_baseline_config_loads_with_the_same_settings() {
    let temp = tempfile::tempdir().unwrap();
    let env = host(temp.path());
    for version in versions("config") {
        for document in documents(&version) {
            let file = document.to_str().unwrap();

            let (code, out, err) = kipple(&env, &["--config", file, "config", "show"]);

            assert_eq!((code, err.as_str()), (ExitCode::SUCCESS, ""), "{file}");
            assert_shows(&document, &out);
        }
    }
}

// Catches a theme a released user wrote refused after an upgrade (a role, colour or
// modifier name dropped, `extends` read differently) or drawn in other styles.
#[test]
fn every_baseline_theme_loads_with_the_same_styles() {
    for version in versions("theme") {
        let temp = tempfile::tempdir().unwrap();
        let env = host(temp.path());
        let dir = Themes::new(&env).dir().unwrap().to_owned();
        fs::create_dir_all(&dir).unwrap();
        let themes = documents(&version);
        for document in &themes {
            fs::copy(document, dir.join(document.file_name().unwrap())).unwrap();
        }
        for document in &themes {
            let name = document.file_stem().unwrap().to_str().unwrap();

            let (code, out, err) = kipple(&env, &["theme", "show", name]);

            assert_eq!((code, err.as_str()), (ExitCode::SUCCESS, ""), "{name}");
            assert_shows(document, &out);
        }
    }
}

// Catches a third-party rule pack written for an earlier release refused after an
// upgrade: a key, selector, fact, predicate form or operation it uses no longer accepted.
#[test]
fn every_baseline_rule_pack_still_validates() {
    let engine = semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
    let integrations = builtin::descriptors();
    let catalog = Catalog {
        engine: &engine,
        integrations: &integrations,
    };
    for version in versions("pack") {
        for document in documents(&version) {
            let file = document.display().to_string();
            let bytes = fs::read(&document).unwrap();

            let validated = pack::validate(&file, &bytes, Origin::ThirdParty, catalog);

            assert!(validated.is_ok(), "{file}: {:?}", validated.err());
        }
    }
}

/// The documents with extension `ext` in the baseline directory `version`.
#[cfg(unix)]
#[expect(
    clippy::unwrap_used,
    reason = "a baseline that can't be read fails the test"
)]
fn documents_of(version: &Path, ext: &str) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(version)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|found| found == ext))
        .collect();
    found.sort();
    found
}

/// One stored run, as the files an earlier release wrote: reads only.
#[cfg(unix)]
#[derive(Debug)]
struct Baseline {
    id: RunId,
    plan: Vec<u8>,
    journal: Vec<u8>,
}

#[cfg(unix)]
impl RunStore for Baseline {
    fn lock(&self, _: &CancelToken) -> Result<Locked, StoreRefusal> {
        Err(StoreRefusal::Unsupported)
    }

    fn lock_state(&self) -> Result<LockState, StoreRefusal> {
        Ok(LockState::Free)
    }

    fn runs(&self) -> Result<Vec<RunId>, ApplyError> {
        Ok(vec![self.id.clone()])
    }

    fn header(&self, _: &RunId) -> Result<Option<Vec<u8>>, ApplyError> {
        let first = self.plan.split_inclusive(|byte| *byte == b'\n').next();
        Ok(Some(first.unwrap_or_default().to_vec()))
    }

    fn plan(&self, _: &RunId) -> Result<Option<Vec<u8>>, ApplyError> {
        Ok(Some(self.plan.clone()))
    }

    fn journal(&self, _: &RunId) -> Result<Vec<u8>, ApplyError> {
        Ok(self.journal.clone())
    }

    fn create(&self, _: &Locked, _: &[u8], _: &[u8]) -> Result<RunId, StoreRefusal> {
        Err(StoreRefusal::Unsupported)
    }

    fn append(&self, _: &Locked, _: &RunId, _: u64, _: &[u8]) -> Result<u64, ApplyError> {
        Err(ApplyError::new(ErrorCode::Unsupported))
    }
}

// Catches a run an earlier release stored unreadable after an upgrade, so `history` can't
// list it and the next mutating command can't reconcile it: each baseline stored plan,
// with the journal of the same name, reads with its trees, and its interrupted item stays
// pending. The names in its trees are a Unix machine's bytes.
#[cfg(unix)]
#[test]
fn every_baseline_run_reads_with_its_pending_item() {
    for version in versions("stored-plan") {
        for document in documents_of(&version, "plan") {
            let name = document.file_name().unwrap();
            let journal = Path::new(BASELINES)
                .join("journal/v1")
                .join(name)
                .with_extension("journal");
            let store = Baseline {
                id: RunId::parse("20261030T000000Z-0000").unwrap(),
                plan: fs::read(&document).unwrap(),
                journal: fs::read(&journal).unwrap(),
            };

            let read = read_run(&store, &store.id, Depth::Trees, &OsNames);
            let runs = history(&store, LockState::Free, &OsNames).unwrap();

            let RunRead::Read(run) = read else {
                panic!("{}: {read:?}", document.display());
            };
            assert_eq!(run.pending().len(), 1, "{}", document.display());
            assert_eq!(
                runs[0].state,
                RunState::Interrupted,
                "{}",
                document.display()
            );
        }
    }
}

// Catches a plan file an earlier release wrote with `plan -o` refused after an upgrade:
// each baseline plan file decodes, every item with it.
#[cfg(unix)]
#[test]
fn every_baseline_plan_file_reads() {
    for version in versions("plan") {
        for document in documents_of(&version, "json")
            .into_iter()
            .filter(|path| !path.ends_with("schema.json"))
        {
            let bytes = fs::read(&document).unwrap();

            let imported = decode_plan_file(&bytes, &OsNames);

            assert_eq!(
                imported.map(|plan| plan.len()),
                Ok(1),
                "{}",
                document.display()
            );
        }
    }
}
