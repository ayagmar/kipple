//! `cargo xtask check-arch`: the crate boundaries from `docs/05-architecture.md` §1, and
//! the binary's one module of words for people from §4.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::cmd;

/// Every dependency allowed between kipple crates. Any other edge is a violation.
/// `xtask` is a dev-only tool and may depend on any crate.
const ALLOWED_EDGES: &[(&str, &str)] = &[
    ("kipple-adapters", "kipple-core"),
    ("kipple-platform", "kipple-core"),
    ("kipple", "kipple-core"),
    ("kipple", "kipple-adapters"),
    ("kipple", "kipple-platform"),
];

const DEV_TOOL: &str = "xtask";

/// The only crate that may contain OS-specific code.
const PLATFORM_CRATE_DIR: &str = "crates/kipple-platform";

/// What the core crate exports: the types whose words for people `human` owns.
const CORE_LIB: &str = "crates/kipple-core/src/lib.rs";

/// The binary, whose frontends render what `human` words.
const BINARY_DIR: &str = "crates/kipple/src";

/// The module that words facts for people, as a file and as a directory.
const HUMAN: [&str; 2] = ["crates/kipple/src/human.rs", "crates/kipple/src/human"];

/// The JSON model, whose names are the wire format's, not words for people.
const WIRE: &str = "crates/kipple/src/json.rs";

/// Frontend types that stand for a core one: the tree's shelf is a bucket, or measuring.
/// A JSON type named `<Core>Out` stands for `<Core>` too.
const MIRRORS: &[(&str, &str)] = &[("Shelf", "Bucket")];

#[derive(Debug, Deserialize)]
struct Metadata {
    packages: Vec<Package>,
}

#[derive(Debug, Deserialize)]
struct Package {
    name: String,
    dependencies: Vec<Dependency>,
}

#[derive(Debug, Deserialize)]
struct Dependency {
    name: String,
    /// `None` for a normal dependency, otherwise `"dev"` or `"build"`.
    kind: Option<String>,
}

/// A Rust source file, with its path relative to the workspace root.
#[derive(Debug)]
pub(crate) struct SourceFile {
    pub(crate) path: PathBuf,
    pub(crate) contents: String,
}

/// Fails if the workspace breaks a crate boundary.
pub(crate) fn run(root: &Path) -> Result<()> {
    let mut violations = edge_violations(&load_metadata(root)?.packages);
    let sources = rust_sources(root, &["crates"])?;
    violations.extend(target_os_violations(&sources));
    violations.extend(wording_violations(&sources));
    if violations.is_empty() {
        return Ok(());
    }
    let mut stderr = io::stderr().lock();
    for violation in &violations {
        writeln!(stderr, "check-arch: {violation}")?;
    }
    bail!(
        "{} architecture violation(s), see docs/05-architecture.md §1 and §4",
        violations.len()
    )
}

fn load_metadata(root: &Path) -> Result<Metadata> {
    let mut command = cmd::command("cargo", root);
    command.args(["metadata", "--format-version", "1", "--no-deps", "--locked"]);
    let json = cmd::stdout(&mut command)?;
    serde_json::from_slice(&json).context("could not parse `cargo metadata` output")
}

/// Dependencies between workspace crates that are not in [`ALLOWED_EDGES`], of any kind.
fn edge_violations(packages: &[Package]) -> Vec<String> {
    let members: BTreeSet<&str> = packages.iter().map(|p| p.name.as_str()).collect();
    let mut violations = Vec::new();
    for package in packages.iter().filter(|p| p.name != DEV_TOOL) {
        for dependency in &package.dependencies {
            let edge = (package.name.as_str(), dependency.name.as_str());
            if members.contains(edge.1) && !ALLOWED_EDGES.contains(&edge) {
                let kind = dependency
                    .kind
                    .as_deref()
                    .map_or(String::new(), |k| format!("{k}-"));
                violations.push(format!(
                    "`{}` has a {kind}dependency on `{}`, which is not an allowed edge",
                    edge.0, edge.1
                ));
            }
        }
    }
    violations.sort();
    violations
}

/// Mentions of `target_os` in any crate other than `kipple-platform`.
fn target_os_violations(files: &[SourceFile]) -> Vec<String> {
    let mut violations = Vec::new();
    for file in files
        .iter()
        .filter(|f| !f.path.starts_with(PLATFORM_CRATE_DIR))
    {
        for (index, line) in file.contents.lines().enumerate() {
            if line.contains("target_os") {
                violations.push(format!(
                    "{}:{}: `target_os` is only allowed in {PLATFORM_CRATE_DIR}",
                    file.path.display(),
                    index + 1
                ));
            }
        }
    }
    violations.sort();
    violations
}

/// Match arms outside `human` that word a core enum for people (`docs/05-architecture.md`
/// §4): an arm whose pattern names a core type, or a mirror of one, and whose value is
/// a string literal or `format!`. Such an arm fails when `human` matches on that type,
/// since `human` then words it, or when a second frontend module words it too, since
/// then the two can drift apart. Arms that map a value to something else, such as a
/// theme role, and test code pass.
fn wording_violations(files: &[SourceFile]) -> Vec<String> {
    let core = files
        .iter()
        .find(|f| f.path == Path::new(CORE_LIB))
        .map(|f| core_exports(&f.contents))
        .unwrap_or_default();
    let core_arms = |file: &SourceFile, text_only: bool| -> Vec<(usize, String)> {
        match_arms(&file.contents)
            .into_iter()
            .filter(|arm| arm.text || !text_only)
            .filter_map(|arm| {
                let name = arm.types.iter().find_map(|name| core_name(name, &core))?;
                Some((arm.line, name))
            })
            .collect()
    };
    let in_human = |file: &&SourceFile| HUMAN.iter().any(|h| file.path.starts_with(h));
    let owned: BTreeSet<String> = files
        .iter()
        .filter(in_human)
        .filter(|f| !is_test(f))
        .flat_map(|f| core_arms(f, false).into_iter().map(|(_, name)| name))
        .collect();
    let frontends: Vec<(&SourceFile, Vec<(usize, String)>)> = files
        .iter()
        .filter(|f| f.path.starts_with(BINARY_DIR) && f.path != Path::new(WIRE))
        .filter(|f| !in_human(f) && !is_test(f))
        .map(|f| (f, core_arms(f, true)))
        .collect();
    let mut modules: BTreeMap<&str, BTreeSet<&Path>> = BTreeMap::new();
    for (file, arms) in &frontends {
        for (_, name) in arms {
            modules.entry(name).or_default().insert(&file.path);
        }
    }
    let mut violations = Vec::new();
    for (file, arms) in &frontends {
        for (line, name) in arms {
            let at = format!("{}:{line}", file.path.display());
            if owned.contains(name) {
                violations.push(format!(
                    "{at}: words `{name}`, which `human` words; render `human`'s words instead"
                ));
            } else if modules.get(name.as_str()).is_some_and(|m| m.len() > 1) {
                violations.push(format!(
                    "{at}: `{name}` is worded in more than one frontend module; word it once \
                     in `human`"
                ));
            }
        }
    }
    violations.sort();
    violations
}

/// Whether `file` holds only tests: a `tests.rs` or a file under a `tests` directory.
fn is_test(file: &SourceFile) -> bool {
    file.path
        .components()
        .any(|part| part.as_os_str() == "tests" || part.as_os_str() == "tests.rs")
}

/// Every name `pub use` exports in the core crate's root.
fn core_exports(lib: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for item in lib.split(';') {
        let Some((_, path)) = item.split_once("pub use ") else {
            continue;
        };
        let listed = path.split_once('{').map_or(path, |(_, list)| list);
        for name in listed.split([',', '{', '}']) {
            let name = name.split_whitespace().last().unwrap_or_default();
            let name = name.rsplit("::").next().unwrap_or(name);
            if name.starts_with(char::is_uppercase) {
                names.insert(name.to_owned());
            }
        }
    }
    names
}

/// The core type `name` is or stands for, if any.
fn core_name(name: &str, core: &BTreeSet<String>) -> Option<String> {
    let name = MIRRORS
        .iter()
        .find(|(mirror, _)| *mirror == name)
        .map_or(name, |(_, stands_for)| stands_for);
    let name = name
        .strip_suffix("Out")
        .filter(|stripped| core.contains(*stripped))
        .unwrap_or(name);
    core.contains(name).then(|| name.to_owned())
}

/// A match arm: its 1-based line, the types its pattern names (`Bucket` in
/// `Some(Bucket::Safe)`), and whether its value is text.
struct Arm {
    line: usize,
    types: Vec<String>,
    text: bool,
}

/// Each match arm of `source` that names a type, outside its inline `#[cfg(test)]`
/// modules.
fn match_arms(source: &str) -> Vec<Arm> {
    let lines: Vec<&str> = source.lines().collect();
    let mut arms = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if let Some(open) = test_module(&lines, index) {
            index = block_end(&lines, open) + 1;
            continue;
        }
        arms.extend(arm(&lines, index));
        index += 1;
    }
    arms
}

/// The line that opens an inline module, if line `index` is a `#[cfg(test)]` before one.
fn test_module(lines: &[&str], index: usize) -> Option<usize> {
    let rest = lines
        .get(index)?
        .trim()
        .strip_prefix("#[cfg(test)]")?
        .trim();
    let (open, item) = if rest.is_empty() {
        (index + 1, lines.get(index + 1)?.trim())
    } else {
        (index, rest)
    };
    (item.starts_with("mod ") && item.ends_with('{')).then_some(open)
}

/// The line on which the block that opens on line `open` closes, by brace depth outside
/// strings, characters and comments; the last line if it never closes.
fn block_end(lines: &[&str], open: usize) -> usize {
    let text = lines.get(open..).unwrap_or_default().join("\n");
    let mut chars = text.char_indices().peekable();
    let mut depth = 0_usize;
    while let Some((at, c)) = chars.next() {
        match c {
            '"' => skip_string(&mut chars),
            'r' => skip_raw_string(&mut chars),
            '\'' => skip_char(&mut chars),
            '/' => skip_comment(&mut chars),
            '{' => depth += 1,
            '}' if depth <= 1 => {
                let before = text.get(..at).unwrap_or_default();
                return open + before.matches('\n').count();
            }
            '}' => depth -= 1,
            _ => {}
        }
    }
    lines.len().saturating_sub(1)
}

type Chars<'a> = std::iter::Peekable<std::str::CharIndices<'a>>;

/// Skips the rest of a string after its opening `"`.
fn skip_string(chars: &mut Chars<'_>) {
    while let Some((_, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '"' => return,
            _ => {}
        }
    }
}

/// Skips a raw string after its `r`, if one starts there: `r"…"` or `r#"…"#`.
fn skip_raw_string(chars: &mut Chars<'_>) {
    let mut hashes = 0;
    while chars.next_if(|&(_, c)| c == '#').is_some() {
        hashes += 1;
    }
    if chars.next_if(|&(_, c)| c == '"').is_none() {
        return;
    }
    while let Some((_, c)) = chars.next() {
        if c == '"' && (0..hashes).all(|_| chars.next_if(|&(_, next)| next == '#').is_some()) {
            return;
        }
    }
}

/// Skips a character literal after its opening `'`, if one starts there rather than a
/// lifetime.
fn skip_char(chars: &mut Chars<'_>) {
    let mut ahead = chars.clone();
    match ahead.next() {
        Some((_, '\\')) => {
            chars.nth(1);
            while chars.next().is_some_and(|(_, c)| c != '\'') {}
        }
        Some(_) if ahead.next().is_some_and(|(_, c)| c == '\'') => *chars = ahead,
        _ => {}
    }
}

/// Skips a comment after its first `/`, if one starts there.
fn skip_comment(chars: &mut Chars<'_>) {
    if chars.next_if(|&(_, c)| c == '/').is_some() {
        while chars.next_if(|&(_, c)| c != '\n').is_some() {}
    } else if chars.next_if(|&(_, c)| c == '*').is_some() {
        let mut star = false;
        for (_, c) in chars.by_ref() {
            if star && c == '/' {
                return;
            }
            star = c == '*';
        }
    }
}

/// The match arm on line `index`, if it has one whose pattern names a type. Its value
/// runs on into the next two lines, for one that starts on the line after `=> {`.
fn arm(lines: &[&str], index: usize) -> Option<Arm> {
    let code = lines.get(index)?.trim();
    if code.starts_with("//") {
        return None;
    }
    let (pattern, value) = code.split_once("=>")?;
    let types = pattern_types(pattern);
    if types.is_empty() {
        return None;
    }
    let following = lines.iter().skip(index + 1).take(2).copied();
    let value: Vec<&str> = std::iter::once(value).chain(following).collect();
    Some(Arm {
        line: index + 1,
        types,
        text: is_text(&value.join(" ")),
    })
}

/// Whether an arm's value is text: a string literal or a `format!`, also behind a block,
/// a `return`, a call such as `Span::styled(` or `Some(`, or `write!(f, `.
fn is_text(value: &str) -> bool {
    let mut rest = value.trim_start();
    loop {
        if ["\"", "r\"", "r#\"", "format!("]
            .iter()
            .any(|start| rest.starts_with(start))
        {
            return true;
        }
        let inner = rest
            .strip_prefix(['{', '('])
            .or_else(|| rest.strip_prefix("return "))
            .or_else(|| written(rest))
            .or_else(|| call_arguments(rest));
        let Some(inner) = inner else {
            return false;
        };
        rest = inner.trim_start();
    }
}

/// What `write!` or `writeln!` at the start of `code` writes: its arguments after the
/// first.
fn written(code: &str) -> Option<&str> {
    let arguments = ["write!(", "writeln!("]
        .iter()
        .find_map(|start| code.strip_prefix(start))?;
    Some(arguments.split_once(',').map_or("", |(_, rest)| rest))
}

/// The arguments of a call to a function or variant named by a path at the start of
/// `code`, as in `Line::from(` or `Some(`.
fn call_arguments(code: &str) -> Option<&str> {
    let (callee, arguments) = code.split_once('(')?;
    callee
        .split("::")
        .all(|part| !part.is_empty() && part.chars().all(|c| c.is_alphanumeric() || c == '_'))
        .then_some(arguments)
}

/// The types a pattern names as `Type::Variant`, without `Self`.
fn pattern_types(pattern: &str) -> Vec<String> {
    pattern
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
        .filter_map(|path| {
            let parts: Vec<&str> = path.split("::").collect();
            let [.., kind, variant] = parts.as_slice() else {
                return None;
            };
            let named = |part: &str| part.starts_with(char::is_uppercase);
            (named(kind) && named(variant) && *kind != "Self").then(|| (*kind).to_owned())
        })
        .collect()
}

/// Every `.rs` file under the directories `dirs` of the workspace.
pub(crate) fn rust_sources(root: &Path, dirs: &[&str]) -> Result<Vec<SourceFile>> {
    let mut files = Vec::new();
    let mut pending: Vec<PathBuf> = dirs.iter().map(|dir| root.join(dir)).collect();
    while let Some(dir) = pending.pop() {
        let entries =
            fs::read_dir(&dir).with_context(|| format!("could not read {}", dir.display()))?;
        for entry in entries {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let contents = fs::read_to_string(&path)
                    .with_context(|| format!("could not read {}", path.display()))?;
                let path = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
                files.push(SourceFile { path, contents });
            }
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(name: &str, dependencies: &[(&str, Option<&str>)]) -> Package {
        Package {
            name: name.to_owned(),
            dependencies: dependencies
                .iter()
                .map(|(dependency, kind)| Dependency {
                    name: (*dependency).to_owned(),
                    kind: kind.map(str::to_owned),
                })
                .collect(),
        }
    }

    fn documented_graph() -> Vec<Package> {
        vec![
            package("kipple-core", &[("serde", None)]),
            package("kipple-platform", &[("kipple-core", None)]),
            package("kipple-adapters", &[("kipple-core", None)]),
            package(
                "kipple",
                &[
                    ("kipple-core", None),
                    ("kipple-platform", None),
                    ("kipple-adapters", None),
                ],
            ),
            package("xtask", &[("kipple", None), ("kipple-core", None)]),
        ]
    }

    fn with_dependency(name: &str, dependency: &str, kind: Option<&str>) -> Vec<Package> {
        let mut packages = documented_graph();
        for package in packages.iter_mut().filter(|p| p.name == name) {
            package.dependencies.push(Dependency {
                name: dependency.to_owned(),
                kind: kind.map(str::to_owned),
            });
        }
        packages
    }

    fn source(path: &str, contents: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from(path),
            contents: contents.to_owned(),
        }
    }

    #[test]
    fn the_documented_crate_graph_passes() {
        assert_eq!(edge_violations(&documented_graph()), Vec::<String>::new());
    }

    #[test]
    fn core_depending_on_platform_fails() {
        let violations = edge_violations(&with_dependency("kipple-core", "kipple-platform", None));
        assert_eq!(violations.len(), 1);
        assert!(
            violations[0].contains("`kipple-core` has a dependency on `kipple-platform`"),
            "{violations:?}"
        );
    }

    #[test]
    fn core_dev_dependency_on_a_kipple_crate_fails() {
        let packages = with_dependency("kipple-core", "kipple-adapters", Some("dev"));
        assert_eq!(edge_violations(&packages).len(), 1);
    }

    #[test]
    fn adapters_and_platform_depending_on_each_other_fails() {
        let mut packages = with_dependency("kipple-adapters", "kipple-platform", None);
        packages.extend(with_dependency(
            "kipple-platform",
            "kipple-adapters",
            Some("build"),
        ));
        let violations = edge_violations(&packages);
        assert!(
            violations
                .iter()
                .any(|v| v.contains("`kipple-adapters` has a dependency")),
            "{violations:?}"
        );
        assert!(
            violations
                .iter()
                .any(|v| v.contains("`kipple-platform` has a build-dependency")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_library_crate_depending_on_the_binary_fails() {
        let packages = with_dependency("kipple-adapters", "kipple", None);
        assert_eq!(edge_violations(&packages).len(), 1);
    }

    #[test]
    fn target_os_outside_platform_fails() {
        let files = [
            source(
                "crates/kipple-core/src/lib.rs",
                "//! Core.\n#[cfg(target_os = \"linux\")]\n",
            ),
            source(
                "crates/kipple/src/main.rs",
                "if cfg!(all(unix, target_os = \"macos\")) {}\n",
            ),
        ];
        let violations = target_os_violations(&files);
        assert_eq!(violations.len(), 2);
        assert!(
            violations
                .iter()
                .any(|v| v.starts_with("crates/kipple-core/src/lib.rs:2:")),
            "{violations:?}"
        );
    }

    #[test]
    fn source_discovery_finds_every_crate_relative_to_the_workspace_root() {
        let files = rust_sources(Path::new(crate::WORKSPACE_ROOT), &["crates"]).unwrap();
        let has = |path: &str| files.iter().any(|f| f.path == Path::new(path));
        assert!(
            has("crates/kipple-core/src/lib.rs"),
            "core's lib.rs is found"
        );
        assert!(
            has("crates/kipple/src/main.rs"),
            "the binary's main.rs is found"
        );
        assert!(
            files.iter().any(|f| f.path.starts_with(PLATFORM_CRATE_DIR)),
            "the platform crate's sources are found"
        );
    }

    /// The core's exports, `human` wording buckets and lenses, and `frontend` at `path`.
    fn binary(path: &str, frontend: &str) -> Vec<SourceFile> {
        vec![
            source(
                CORE_LIB,
                "pub use policy::{\n    Bucket, Hold, Level,\n};\npub use rule::Lens;\n",
            ),
            source(
                "crates/kipple/src/human.rs",
                "match bucket {\n    Some(Bucket::Safe) => Standing {\n        title: \"Safe to clean\",\n    },\n}\nmatch lens {\n    Lens::Agents => \"Agents\",\n}\n",
            ),
            source(path, frontend),
        ]
    }

    // Catches the tree wording buckets again beside `human`, as `shelf_title` did, so
    // the tree and the CLI could name one bucket two ways.
    #[test]
    fn a_frontend_wording_a_bucket_human_words_fails() {
        let violations = wording_violations(&binary(
            "crates/kipple/src/tui/ui.rs",
            "const fn shelf_title(shelf: Shelf) -> &'static str {\n    match shelf {\n        Shelf::Bucket(Bucket::Safe) => \"Safe to clean\",\n        Shelf::Measuring => \"Measuring\",\n    }\n}\n",
        ));
        assert_eq!(violations.len(), 2, "{violations:?}");
        assert!(
            violations[0].starts_with("crates/kipple/src/tui/ui.rs:3: words `Bucket`"),
            "{violations:?}"
        );
    }

    // Catches a mirror slipping through: the JSON `LensOut` and a value on the line
    // after `=> {` word the lens all the same.
    #[test]
    fn a_json_mirror_and_a_block_value_count_as_the_core_type() {
        let violations = wording_violations(&binary(
            "crates/kipple/src/scan.rs",
            "match lens {\n    LensOut::Dev => {\n        \"Dev\".to_owned()\n    }\n    LensOut::System => format!(\"System\"),\n}\n",
        ));
        assert_eq!(violations.len(), 2, "{violations:?}");
        assert!(
            violations.iter().all(|v| v.contains("words `Lens`")),
            "{violations:?}"
        );
    }

    // Catches one fact worded in two frontends before `human` words it, such as the
    // scan's outcome in the title and in the first run's header.
    #[test]
    fn a_core_enum_worded_in_two_frontend_modules_fails_and_in_one_passes() {
        let arm = "match level {\n    Level::Careful => \"Careful\",\n}\n";
        let mut files = binary("crates/kipple/src/tui/ui/frame.rs", arm);
        assert_eq!(wording_violations(&files), Vec::<String>::new());

        files.push(source("crates/kipple/src/tui/ui/onboarding.rs", arm));
        let violations = wording_violations(&files);
        assert_eq!(violations.len(), 2, "{violations:?}");
        assert!(
            violations[0].contains("more than one frontend module"),
            "{violations:?}"
        );
    }

    // Catches wording after an inline test module passing: only the module's own body is
    // test code, even when it holds braces in strings, characters and comments.
    #[test]
    fn wording_after_a_test_module_fails() {
        let violations = wording_violations(&binary(
            "crates/kipple/src/tui/ui.rs",
            "#[cfg(test)]\nmod tests {\n    fn f() {\n        let _ = (\"}\", '}', r#\"}\"#, ('\\'','{')); // }\n        /* } */\n    }\n    Bucket::Safe => \"Safe\",\n}\n\nmatch bucket {\n    Bucket::Safe => \"Wrong\",\n}\n",
        ));
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations[0].starts_with("crates/kipple/src/tui/ui.rs:11: words `Bucket`"),
            "{violations:?}"
        );
    }

    // Catches wording a frontend builds as a span, a line, a `String` or a `write!`, or
    // returns from a block, passing because the arm's value doesn't start with a literal.
    #[test]
    fn text_behind_a_call_or_a_return_counts() {
        let violations = wording_violations(&binary(
            "crates/kipple/src/tui/ui/tree.rs",
            "match shelf {\n    Shelf::SAFE => Span::styled(\"Safe to clean\", s),\n    Bucket::Review => { return \"Review\"; }\n    Bucket::Protected => {\n        return Line::from(\"Protected\");\n    }\n    Lens::Agents => Text::styled(\n        \"Agents\",\n        s,\n    ),\n    Lens::Dev => String::from(\"Dev\"),\n    Lens::System => write!(f, \"System\"),\n    Lens::Other => writeln!(f, \"Other\"),\n    Lens::Mine => Some(Cow::Borrowed(\"Mine\")),\n    Bucket::Safe => Span::styled(human::standing(Some(b)).title, s),\n    Bucket::Safe => Role::Safe,\n}\n",
        ));
        let lines: BTreeSet<u32> = violations
            .iter()
            .map(|v| v.split(':').nth(1).unwrap().parse().unwrap())
            .collect();
        assert_eq!(
            lines,
            BTreeSet::from([2, 3, 4, 7, 11, 12, 13, 14]),
            "{violations:?}"
        );
    }

    // Catches the check failing on what isn't wording: an arm to a theme role, a type's
    // own `Self` arms, a non-core enum, the JSON wire names and test code.
    #[test]
    fn roles_self_arms_other_enums_wire_names_and_tests_pass() {
        let mut files = binary(
            "crates/kipple/src/tui/ui.rs",
            "match shelf {\n    Shelf::Bucket(Bucket::Safe) => Role::Safe,\n}\nmatch self {\n    Self::Flag => \"--config\",\n}\nmatch key {\n    Key::Enter => \"enter\",\n}\n// Bucket::Safe => \"Safe\"\n#[cfg(test)]\nmod tests {\n    Bucket::Safe => \"Safe\",\n}\n",
        );
        files.push(source(WIRE, "Lens::Agents => \"agents\",\n"));
        files.push(source(
            "crates/kipple/src/tui/app/tests/tree.rs",
            "Bucket::Safe => \"Safe\",\n",
        ));
        assert_eq!(wording_violations(&files), Vec::<String>::new());
    }

    // Catches `human` changing shape (a table, a struct of words) so that the check
    // no longer sees it word buckets, levels or lenses, and the workspace drifting from
    // the rule: the real sources pass, and a `shelf_title` put back in the tree fails.
    #[test]
    fn the_workspace_passes_and_fails_with_shelf_title_put_back() {
        let mut files = rust_sources(Path::new(crate::WORKSPACE_ROOT), &["crates"]).unwrap();
        assert_eq!(wording_violations(&files), Vec::<String>::new());

        files.push(source(
            "crates/kipple/src/tui/ui/titles.rs",
            "match shelf {\n    Shelf::SAFE => \"Safe to clean\",\n    Shelf::Measuring => \"Measuring\",\n}\nmatch level {\n    Level::Careful => \"Careful\",\n}\nmatch lens {\n    Lens::Dev => \"Dev\",\n}\n",
        ));
        files.push(source(
            "crates/kipple/src/tui/ui/shelf.rs",
            "match shelf {\n    Shelf::SAFE => Span::styled(\"Safe to clean\", s),\n}\n",
        ));
        let violations = wording_violations(&files);
        assert_eq!(violations.len(), 5, "{violations:?}");
        assert!(
            violations.iter().all(|v| v.contains("which `human` words")),
            "{violations:?}"
        );
    }

    #[test]
    fn target_os_inside_platform_passes() {
        let files = [source(
            "crates/kipple-platform/src/linux.rs",
            "#[cfg(target_os = \"linux\")]\n",
        )];
        assert_eq!(target_os_violations(&files), Vec::<String>::new());
    }
}
