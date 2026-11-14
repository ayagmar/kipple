//! Where `npm.cache` resolves, read as npm 11.20.0's `@npmcli/config` reads it (spec M4
//! pinned 33, S5 npm §2): its variables, then the user npmrc, the global npmrc and npm's
//! own, through an npm install found on `PATH` whose files alone are read. Unix only:
//! `crates/kipple/tests/npm_roots.rs` places it from each OS's installer layout, Windows's
//! `npm.cmd` and `%LOCALAPPDATA%` included.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "a temp dir or path that can't be made fails the test"
)]

#[path = "../../kipple-adapters/tests/fixtures/tree.rs"]
mod tree;

#[path = "../../kipple-adapters/tests/fixtures/npm/v11_20_0.rs"]
#[expect(
    dead_code,
    reason = "these tests use the install and the cache, not the node"
)]
mod npm_v11_20_0;

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use kipple_core::{RootSource, SymbolicRoot};
use kipple_platform::{Environment, resolve_roots};
use tree::FixtureTree;

// `tree.rs` keeps `dead_code` off the builder methods most binaries leave unused, but not
// this one, which every other binary that includes it uses.
const _: () = {
    let _ = FixtureTree::modified;
};

/// npm's own npmrc, below the temp dir.
const BUILTIN: &str = "npm/lib/node_modules/npm/npmrc";

/// The global npmrc npm reads by default, below its node's prefix.
const GLOBAL: &str = "npm/etc/npmrc";

/// A token as an npmrc holds one beside the cache.
const TOKEN: &str = "npm_kippleTestTokenThatMustNeverShow0123";

/// A home and an npm 11.20.0 install in a temp dir, with `PATH` leading to the install.
struct Host {
    _temp: tempfile::TempDir,
    /// The temp dir, physical.
    at: PathBuf,
    home: PathBuf,
    prefix: PathBuf,
}

impl Host {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let at = temp.path().canonicalize().unwrap();
        let mut tree = FixtureTree::new();
        tree.dir("home");
        npm_v11_20_0::install(&mut tree, "npm");
        tree.file("npm/bin/node", b"");
        tree.build_in(&at).unwrap();
        let prefix = at.join("npm");
        let script = prefix.join("lib/node_modules/npm/bin/npm-cli.js");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            home: at.join("home"),
            _temp: temp,
            at,
            prefix,
        }
    }

    /// `path` below the temp dir, as text.
    fn path(&self, path: &str) -> String {
        self.at.join(path).to_str().unwrap().to_owned()
    }

    /// Writes `text` to `path` below the temp dir, making its parents.
    fn write(&self, path: &str, text: &str) {
        let at = self.at.join(path);
        fs::create_dir_all(at.parent().unwrap()).unwrap();
        fs::write(at, text).unwrap();
    }

    /// Where `npm.cache` resolves with `HOME`, `PATH` and `vars` set, or why it doesn't.
    fn cache(&self, vars: &[(&str, &str)]) -> Result<(PathBuf, RootSource), String> {
        let bin = self.prefix.join("bin");
        let base = [
            ("HOME", self.home.to_str().unwrap()),
            ("PATH", bin.to_str().unwrap()),
        ];
        let unset = base
            .into_iter()
            .filter(|(name, _)| vars.iter().all(|(set, _)| set != name));
        let all: Vec<_> = unset.chain(vars.iter().copied()).collect();
        let env = Environment::from_vars(
            all.iter()
                .map(|(k, v)| (OsString::from(k), OsString::from(v))),
        )
        .with_system_root(PathBuf::from("/nonexistent/kipple-test-system"));
        let roots = resolve_roots(&env, &[]);
        match roots.get(SymbolicRoot::NpmCache) {
            Some(Ok(resolved)) => Ok((resolved.path.clone(), resolved.source)),
            Some(Err(unresolved)) => Err(unresolved.to_string()),
            None => Err("not on this platform".to_owned()),
        }
    }
}

const fn setting(file: &'static str) -> RootSource {
    RootSource::Setting { file, key: "cache" }
}

/// Catches the order npm reads its settings in taken wrongly: a cache read from a file npm
/// reads later, or the default kept when a setting moved it, leaves the real cache open
/// and closes a directory npm never writes.
#[test]
fn root_follows_env_then_user_then_global() {
    let host = Host::new();
    let default = (
        host.home.join(".npm"),
        RootSource::Default {
            base: SymbolicRoot::Home,
            relative: ".npm",
        },
    );
    assert_eq!(host.cache(&[]), Ok(default));

    host.write(BUILTIN, &format!("cache={}\n", host.path("builtin")));
    let builtin = (host.at.join("builtin"), setting("npm's own npmrc"));
    assert_eq!(host.cache(&[]), Ok(builtin));

    host.write(GLOBAL, &format!("cache = {}\n", host.path("global")));
    let global = (host.at.join("global"), setting("the global npmrc"));
    assert_eq!(host.cache(&[]), Ok(global));

    host.write("home/.npmrc", "; the user's\ncache=~/user-cache\n");
    let user = (host.home.join("user-cache"), setting("the user npmrc"));
    assert_eq!(host.cache(&[]), Ok(user));

    let moved = host.path("env");
    let env = (host.at.join("env"), RootSource::Env("npm_config_cache"));
    assert_eq!(host.cache(&[("NPM_CONFIG_CACHE", &moved)]), Ok(env));
}

/// Catches the global npmrc placed only below node's prefix: Arch's npm names
/// `/etc/npmrc` in its own npmrc, so a cache set there would be missed.
#[test]
fn globalconfig_in_builtin_names_the_global_file() {
    let host = Host::new();
    let named = host.path("etc/npmrc");
    host.write(BUILTIN, &format!("globalconfig={named}\n"));
    host.write("etc/npmrc", &format!("cache={}\n", host.path("named")));
    host.write(GLOBAL, &format!("cache={}\n", host.path("unread")));

    let found = host.cache(&[]);

    assert_eq!(
        found,
        Ok((host.at.join("named"), setting("the global npmrc")))
    );
}

/// Catches `PREFIX` or `DESTDIR` ignored: either moves the global npmrc npm reads by
/// default, which kipple doesn't model, so a cache set there could be missed.
#[test]
fn prefix_or_destdir_in_the_environment_is_unknown() {
    let host = Host::new();
    let elsewhere = host.path("elsewhere");
    for var in ["PREFIX", "DESTDIR"] {
        let found = host.cache(&[(var, &elsewhere)]);
        let error = found.unwrap_err();
        assert!(error.contains("PREFIX or DESTDIR"), "{var}: {error}");
    }

    let named = host.path("etc/npmrc");
    host.write(BUILTIN, &format!("globalconfig={named}\n"));
    let found = host.cache(&[("PREFIX", &elsewhere)]);
    assert_eq!(
        found.map(|(path, _)| path),
        Ok(host.home.join(".npm")),
        "a global npmrc named outright doesn't depend on the prefix"
    );
}

/// Catches one spelling of a variable picked over another: npm takes whichever the system
/// lists last, so two values that disagree can't be placed.
#[test]
fn disagreeing_env_spellings_block() {
    let host = Host::new();
    let (a, b) = (host.path("a"), host.path("b"));

    let disagree = host.cache(&[("npm_config_cache", &a), ("NPM_CONFIG_CACHE", &b)]);
    let agree = host.cache(&[("npm_config_cache", &a), ("Npm_Config_Cache", &a)]);
    let empty = host.cache(&[("npm_config_cache", &a), ("NPM_CONFIG_CACHE", "")]);

    assert!(
        disagree.unwrap_err().contains("two spellings"),
        "two values hold"
    );
    assert_eq!(agree.map(|(path, _)| path), Ok(host.at.join("a")));
    assert_eq!(
        empty.map(|(path, _)| path),
        Ok(host.at.join("a")),
        "empty is unset"
    );
}

/// Catches a value npm would read against its working directory, or with a reference
/// left as written, placed as a path: either names a directory npm doesn't write.
#[test]
fn relative_and_unset_values_block() {
    let host = Host::new();
    let refused = [
        "cache=relative/cache\n",
        "cache=${KIPPLE_TEST_UNSET}/cache\n",
        "cache=${KIPPLE_TEST_UNSET?}cache\n",
        "cache\n",
        "cache=true\n",
        "cache[]=/a\n",
        "cache=\"/quoted\\\\path\"\n",
        "cache=/a\\b\n",
        "cache = \"~/c\" ; npm keeps the quotes\n",
        "userconfig=relative\n",
        "prefix=relative\n",
    ];
    for npmrc in refused {
        let file = if npmrc.starts_with("userconfig") {
            BUILTIN
        } else {
            "home/.npmrc"
        };
        host.write(file, npmrc);
        let found = host.cache(&[]);
        assert!(found.is_err(), "{npmrc:?} holds, found {found:?}");
        host.write(file, "");
    }

    let accepted = [
        ("cache=${HOME}/a\n", "a"),
        ("cache=${KIPPLE_TEST_UNSET?}${HOME}/b\n", "b"),
        ("cache = ~/c ; a comment\n", "c"),
        ("cache = \"~/q\"\n", "q"),
        ("cache=~/d/../e/./f\n", "e/f"),
        (
            "cache=/elsewhere\n[section]\ncache=/not-this\n",
            "../elsewhere",
        ),
    ];
    for (npmrc, below) in accepted {
        host.write("home/.npmrc", npmrc);
        let found = host.cache(&[]).map(|(path, _)| path);
        let expected = if below.starts_with("..") {
            PathBuf::from("/elsewhere")
        } else {
            host.home.join(below)
        };
        assert_eq!(found, Ok(expected), "{npmrc:?}");
    }
}

/// Catches a cache placed where npm, reading the same file, could put it elsewhere (the
/// fix round's review ran npm's own `ini` and `env-replace` on each): a key that expands to
/// `cache` wins or loses by the order `ini` stored the keys in, a key that was once a list
/// stays one, and white space a variable adds is kept, making the path relative.
#[test]
fn where_npm_could_read_another_cache_it_is_unknown() {
    let host = Host::new();
    let vars = [("K", "cache"), ("PAD", " /padded"), ("TAIL", "/tail ")];
    let refused = [
        "cache=/b\n${K}=/a\ncache=/c\n",
        "${K}=/a\n",
        "${KIPPLE_TEST_UNSET?}cache=/a\n",
        "${K}[]=/a\n",
        "cache[]=/a\ncache=/b\n",
        "cache=/a\ncache[]=/b\n",
        "cache=${PAD}\n",
        "cache=${TAIL}\n",
    ];
    for npmrc in refused {
        host.write("home/.npmrc", npmrc);
        let found = host.cache(&vars);
        assert!(found.is_err(), "{npmrc:?} holds, found {found:?}");
    }
    let padded = [("npm_config_cache", "${PAD}"), ("PAD", " /lead")];
    host.write("home/.npmrc", "");
    let from_env = host.cache(&padded);
    assert!(from_env.is_err(), "a padded variable holds: {from_env:?}");

    let elsewhere = [("K", "elsewhere"), ("P", "/p")];
    host.write("home/.npmrc", "${K}=/a\ncache=${P}\n");
    let found = host.cache(&elsewhere).map(|(path, _)| path);
    assert_eq!(found, Ok(PathBuf::from("/p")), "a key npm reads elsewhere");
}

/// Catches an npmrc kipple can't read treated as one that sets nothing, which would leave
/// a moved cache open.
#[test]
fn an_unreadable_huge_or_unparsable_npmrc_is_unknown() {
    let host = Host::new();
    host.write("home/.npmrc", "cache=/elsewhere\n");
    let npmrc = host.home.join(".npmrc");
    fs::set_permissions(&npmrc, fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = host.cache(&[]);
    // Root reads any file, so only a user it stops can see this.
    let stopped = fs::File::open(&npmrc).is_err();
    fs::set_permissions(&npmrc, fs::Permissions::from_mode(0o600)).unwrap();
    if stopped {
        let shown = unreadable.unwrap_err();
        assert!(shown.contains("the user npmrc can't be read"), "{shown}");
    }

    let mut huge = "; padding\n".repeat(30_000);
    huge.push_str("cache=/elsewhere\n");
    host.write("home/.npmrc", &huge);
    let too_large = host.cache(&[]).unwrap_err();
    assert!(
        too_large.contains("the user npmrc can't be read"),
        "{too_large}"
    );

    host.write("home/.npmrc", "cache=/elsewhere\u{2028}\n");
    let unparsable = host.cache(&[]).unwrap_err();
    assert!(
        unparsable.contains("can't be read the way npm reads it"),
        "{unparsable}"
    );

    let dir = host.path("a-directory");
    fs::create_dir(&dir).unwrap();
    assert!(
        host.cache(&[("npm_config_userconfig", &dir)]).is_err(),
        "a directory where the npmrc is holds"
    );
}

/// Catches a cache placed for an npm kipple never found: its own npmrc may move it, so
/// only a variable can place it then.
#[test]
fn without_npm_on_path_only_a_variable_places_the_cache() {
    let host = Host::new();
    let empty = host.at.join("empty");
    fs::create_dir(&empty).unwrap();
    let bare = empty.to_str().unwrap();

    let unknown = host.cache(&[("PATH", bare)]);
    let moved = host.path("env");
    let set = host.cache(&[("PATH", bare), ("npm_config_cache", &moved)]);

    assert!(
        unknown.unwrap_err().contains("wasn't found on PATH"),
        "PATH holds no npm"
    );
    assert_eq!(set.map(|(path, _)| path), Ok(host.at.join("env")));
}

/// Catches a token, or any other value of an npmrc, reaching what kipple shows: `doctor`
/// prints why a root is unknown, and npmrc files hold credentials.
#[test]
fn a_token_beside_the_cache_reaches_no_error() {
    let host = Host::new();
    let lines = [
        format!("//registry.npmjs.org/:_authToken={TOKEN}\ncache=relative-{TOKEN}\n"),
        format!("//registry.npmjs.org/:_authToken={TOKEN}\ncache=${{{TOKEN}}}/c\n"),
        format!("_auth={TOKEN}\nprefix={TOKEN}\n"),
        format!("{TOKEN}\u{2028}\n"),
    ];
    for npmrc in &lines {
        host.write("home/.npmrc", npmrc);
        let found = host.cache(&[]);
        let error = found.as_ref().err().map(ToString::to_string);
        let shown = format!("{found:?} {error:?}");
        assert!(error.is_some(), "{npmrc:?} holds");
        assert!(!shown.contains(TOKEN), "the token shows: {shown}");
        assert!(!shown.contains("relative-"), "the value shows: {shown}");
    }
    let found = host.cache(&[("npm_config_cache", &format!("rel/{TOKEN}"))]);
    let shown = format!(
        "{found:?} {:?}",
        found.as_ref().err().map(ToString::to_string)
    );
    assert!(
        !shown.contains(TOKEN),
        "the variable's value shows: {shown}"
    );
}

/// Catches the parser panicking, or placing a cache from text npm wouldn't read as one,
/// on input nobody wrote by hand: each generated npmrc either holds, or places the cache
/// at an absolute path from its own `cache=` line.
#[test]
fn npmrc_survives_generated_input() {
    let host = Host::new();
    let pieces = [
        "cache",
        "=",
        " ",
        "/",
        "a",
        "~/",
        "${HOME}",
        "${X?}",
        "\\",
        ";",
        "#",
        "\"",
        "'",
        "[",
        "]",
        "\r",
        "\n",
        "\u{feff}",
        "\u{2028}",
        "\t",
        "..",
        "prefix",
        "globalconfig",
        "userconfig",
        "true",
        "[]",
        "é",
        "\0",
        "${K}",
        "${PAD}",
        "\n${K}=/k\n",
        "\ncache[]=/l\n",
        "\ncache=/s\n",
        "\ncache=${PAD}\n",
    ];
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let count = u64::try_from(pieces.len()).unwrap();
    for _ in 0..400 {
        let len = next() % 24;
        let text: String = (0..len)
            .map(|_| pieces[usize::try_from(next() % count).unwrap()])
            .collect();
        host.write("home/.npmrc", &text);
        let found = host.cache(&[("K", "cache"), ("PAD", "/p ")]);
        assert!(
            !(must_hold(&text) && found.is_ok()),
            "{text:?} could be another cache to npm, placed {found:?}"
        );
        if let Ok((path, _)) = found {
            assert!(path.is_absolute(), "{text:?} placed {path:?}");
            let default = host.home.join(".npm");
            let set = text.contains("cache");
            assert!(
                set || path == default,
                "{text:?} placed {path:?} with no cache line"
            );
        }
    }
}

/// Whether generated `text`, with `K=cache` and `PAD="/p "`, holds a line on which npm and
/// kipple could disagree about the cache, before anything that could end the top level:
/// a key that expands to `cache`, a scalar after a list, or a padded last value.
fn must_hold(text: &str) -> bool {
    let lines: Vec<&str> = text.split(['\r', '\n']).collect();
    let top = lines.iter().take_while(|line| !line.starts_with('['));
    let top: Vec<&str> = top.copied().collect();
    let at = |wanted: &str| top.iter().position(|line| *line == wanted);
    let expanded_key = at("${K}=/k").is_some();
    let list_then_scalar =
        at("cache[]=/l").is_some_and(|list| top.iter().skip(list).any(|line| *line == "cache=/s"));
    let padded_last = at("cache=${PAD}").is_some_and(|padded| {
        lines
            .iter()
            .skip(padded + 1)
            .all(|line| !line.contains("cache") && !line.contains("${K"))
    });
    expanded_key || list_then_scalar || padded_last
}
