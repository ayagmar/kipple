//! The rule-pack validator against the schema v1 contract of
//! `docs/06-adapters-and-rules.md` §3: a valid pack comes through whole, and every
//! documented error case is refused with its documented code at the offending line.

use kipple_adapters::pack::{Catalog, ErrorCode, Origin, PackError, validate};
use kipple_core::{
    Bounds, IntegrationDescriptor, IntegrationId, IntegrationSelector, Lens, Operation, Pack,
    Platform, Selector, SymbolicRoot, Test,
};

/// A third-party pack that uses every part of the schema it may use.
const VALID: &str = r#"schema_version = 1

[pack]
id = "dev.example.tools"
version = "1.2.0"
publisher = "Example"
description = "Example rules"
license = "MIT"
kipple = ">=0.0.0"
platforms = ["linux", "macos"]

[[rule]]
id = "dev.example.tools.bun-cache"
integration = "generic"
title = "Bun install cache"
why = "Bun keeps downloaded packages here."
loss = "Bun downloads them again."
references = ["https://bun.sh/docs/install/cache"]
category = "cache"

[rule.scope]
root = "home"
selection = "dirs-matching"
pattern = ".bun/install/cache"

[rule.eligibility]
modified_age = { min = "30d" }
size = { min = "100MB", max = "10GiB" }
git.tracked_files_in_item = "absent"
activity = ["idle"]

[rule.operation]
kind = "remove-files"
methods = ["delete", "trash"]

[rule.selection]
default = "unselected"
recent_days = 14
"#;

const PYTHON: IntegrationDescriptor = IntegrationDescriptor {
    id: IntegrationId("python"),
    lens: Lens::Dev,
    platforms: &[Platform::Linux, Platform::Macos, Platform::Windows],
    roots: &[],
    selectors: &[IntegrationSelector {
        name: "bytecode-caches",
        meaning: "test selector",
    }],
    artifacts: &[],
    owner_files: &[],
    verified: &[],
    consequences: &[],
    revision: 1,
};

const CARGO: IntegrationDescriptor = IntegrationDescriptor {
    id: IntegrationId("cargo"),
    selectors: &[],
    ..PYTHON
};

fn check(text: &str, origin: Origin) -> Result<Pack, Vec<PackError>> {
    let engine = semver::Version::new(0, 4, 0);
    let integrations = [PYTHON, CARGO];
    let catalog = Catalog {
        engine: &engine,
        integrations: &integrations,
    };
    validate("pack.toml", text.as_bytes(), origin, catalog)
}

/// `VALID` with each `(from, to)` replaced once, which must apply.
fn with(edits: &[(&str, &str)]) -> String {
    let mut text = VALID.to_owned();
    for (from, to) in edits {
        assert!(text.contains(from), "{from:?} is not in the pack");
        text = text.replacen(from, to, 1);
    }
    text
}

/// The codes of every error for `text`, which must be refused.
#[expect(clippy::panic, reason = "an accepted pack fails the test")]
fn codes(text: &str, origin: Origin) -> Vec<ErrorCode> {
    match check(text, origin) {
        Ok(pack) => panic!("accepted:\n{text}\n{pack:?}"),
        Err(errors) => errors.into_iter().map(|e| e.code).collect(),
    }
}

/// Asserts that each document is refused with exactly `code`.
fn refused(code: ErrorCode, documents: &[String]) {
    for text in documents {
        assert_eq!(codes(text, Origin::ThirdParty), [code], "\n{text}");
    }
}

#[test]
fn a_valid_pack_comes_through_with_its_defaults() {
    let pack = check(VALID, Origin::ThirdParty).unwrap();

    assert_eq!(pack.id.0, "dev.example.tools");
    assert_eq!(pack.source, None);
    let [rule] = pack.rules.as_slice() else {
        panic!("{:?}", pack.rules);
    };
    assert_eq!(rule.integration, IntegrationId::GENERIC);
    assert_eq!(rule.platforms, [Platform::Linux, Platform::Macos]);
    assert_eq!(rule.root, SymbolicRoot::Home);
    assert_eq!(rule.text.description, "");
    assert_eq!(rule.recent_days, Some(14));
    let Selector::DirsMatching {
        pattern,
        max_matches,
    } = &rule.selector
    else {
        panic!("{:?}", rule.selector);
    };
    assert_eq!(pattern.0, [".bun", "install", "cache"]);
    assert_eq!(*max_matches, 1000);
    let tests: Vec<(&str, &Test)> = rule.eligibility.iter().map(|p| (p.fact, &p.test)).collect();
    assert!(
        tests.contains(&(
            "size",
            &Test::Bytes(Bounds {
                min: Some(100_000_000),
                max: Some(10 * (1 << 30)),
            })
        )),
        "the default size bound is kept"
    );
    assert!(
        tests.contains(&(
            "git.tracked_files_in_item",
            &Test::Count(Bounds {
                min: Some(0),
                max: Some(0),
            })
        )),
        "the default tracked-files bound is kept"
    );
    assert!(
        matches!(&rule.operation, Operation::RemoveFiles(methods) if methods.len() == 2),
        "both removal methods are kept"
    );
}

#[test]
fn errors_point_at_the_offending_key_with_file_line_and_column() {
    let text = with(&[(
        "category = \"cache\"",
        "category = \"cache\"\ncolour = \"blue\"",
    )]);

    let errors = check(&text, Origin::ThirdParty).unwrap_err();

    let [error] = errors.as_slice() else {
        panic!("{errors:?}");
    };
    assert_eq!(error.code, ErrorCode::UnknownKey);
    let line = text.lines().position(|l| l.starts_with("colour")).unwrap() + 1;
    assert_eq!(
        (error.file.as_str(), error.line, error.column),
        ("pack.toml", line, 1)
    );
    assert!(
        error
            .to_string()
            .starts_with(&format!("pack.toml:{line}:1: unknown-key")),
        "the error names the file, line and column: {error}"
    );
}

#[test]
fn every_problem_in_a_pack_is_reported_not_just_the_first() {
    let text = with(&[
        ("license = \"MIT\"\n", ""),
        ("title = \"Bun install cache\"", "title = 7"),
    ]);

    assert_eq!(
        codes(&text, Origin::ThirdParty),
        [ErrorCode::MissingKey, ErrorCode::WrongType]
    );
}

#[test]
fn a_document_that_is_not_utf8_toml_is_a_parse_error() {
    let not_utf8 = [VALID.as_bytes(), b"# \xff\xfe\n"].concat();
    let engine = semver::Version::new(0, 4, 0);
    let catalog = Catalog {
        engine: &engine,
        integrations: &[],
    };

    let errors = validate("pack.toml", &not_utf8, Origin::ThirdParty, catalog).unwrap_err();

    assert_eq!(errors[0].code, ErrorCode::ParseError);
    refused(ErrorCode::ParseError, &[with(&[("[pack]", "[pack")])]);
}

#[test]
fn a_document_over_256_kib_is_too_large() {
    let padding = format!("# {}\n", "x".repeat(256 * 1024));

    refused(ErrorCode::TooLarge, &[format!("{VALID}{padding}")]);
}

#[test]
fn only_schema_version_1_is_read() {
    refused(
        ErrorCode::UnsupportedSchema,
        &[
            with(&[("schema_version = 1", "schema_version = 2")]),
            with(&[("schema_version = 1", "schema_version = 0")]),
        ],
    );
    refused(
        ErrorCode::WrongType,
        &[with(&[("schema_version = 1", "schema_version = \"1\"")])],
    );
}

#[test]
fn unknown_keys_are_refused_at_every_level() {
    refused(
        ErrorCode::UnknownKey,
        &[
            with(&[("schema_version = 1", "schema_version = 1\nextra = 1")]),
            with(&[("license = \"MIT\"", "license = \"MIT\"\nhomepage = \"x\"")]),
            with(&[("pattern = ", "recursive = true\npattern = ")]),
            with(&[("{ min = \"30d\" }", "{ min = \"30d\", avg = \"40d\" }")]),
            with(&[("recent_days = 14", "recent_days = 14\nwhen = 1")]),
            with(&[(
                "[rule.selection]",
                "[rule.retention]\nkeep_additional = 1\n\n[rule.selection]",
            )]),
            with(&[(
                "kind = \"remove-files\"\nmethods = [\"delete\", \"trash\"]",
                "kind = \"report\"\nmethods = [\"delete\"]",
            )]),
        ],
    );
}

#[test]
fn required_keys_must_be_present() {
    refused(
        ErrorCode::MissingKey,
        &[
            with(&[("license = \"MIT\"\n", "")]),
            with(&[("why = \"Bun keeps downloaded packages here.\"\n", "")]),
            with(&[("selection = \"dirs-matching\"\n", "")]),
            with(&[("pattern = \".bun/install/cache\"\n", "")]),
            with(&[("methods = [\"delete\", \"trash\"]\n", "")]),
            VALID.split("[[rule]]").next().unwrap().to_owned(),
            with(&[(
                "selection = \"dirs-matching\"\npattern = \".bun/install/cache\"",
                "selection = \"project-artifact\"\nmarker = [\"package.json\"]",
            )]),
        ],
    );
}

#[test]
fn values_of_the_wrong_type_or_predicates_of_the_wrong_form_are_refused() {
    refused(
        ErrorCode::WrongType,
        &[
            with(&[("title = \"Bun install cache\"", "title = 7")]),
            with(&[("category = \"cache\"", "category = \"junk\"")]),
            with(&[(
                "platforms = [\"linux\", \"macos\"]",
                "platforms = [\"beos\"]",
            )]),
            with(&[("activity = [\"idle\"]", "activity = { min = \"1d\" }")]),
            with(&[("activity = [\"idle\"]", "activity = \"sleeping\"")]),
            with(&[("{ min = \"30d\" }", "\"30d\"")]),
            with(&[("{ min = \"30d\" }", "{ min = \"30 days\" }")]),
            with(&[("{ min = \"100MB\", max = \"10GiB\" }", "{ min = 100 }")]),
            with(&[(
                "{ min = \"100MB\", max = \"10GiB\" }",
                "{ min = \"100 MB\" }",
            )]),
            with(&[(
                "tracked_files_in_item = \"absent\"",
                "tracked_files_in_item = \"none\"",
            )]),
            with(&[("https://bun.sh/docs/install/cache", "http://bun.sh/docs")]),
            with(&[("default = \"unselected\"", "default = \"selected\"")]),
            with(&[("methods = [\"delete\", \"trash\"]", "methods = [\"shred\"]")]),
        ],
    );
}

#[test]
fn strings_over_their_limit_or_with_control_characters_are_refused() {
    let title = format!("title = \"{}\"", "t".repeat(201));
    refused(
        ErrorCode::StringTooLong,
        &[
            with(&[("title = \"Bun install cache\"", &title)]),
            with(&[("publisher = \"Example\"", "publisher = \"Ex\\u0007ample\"")]),
            with(&[("why = \"Bun keeps", "why = \"Line one\\nBun keeps")]),
        ],
    );
}

#[test]
fn ids_must_follow_the_grammar_and_rule_ids_their_pack() {
    let long = format!("id = \"dev.{}\"", "a".repeat(61));
    refused(
        ErrorCode::InvalidId,
        &[
            with(&[("id = \"dev.example.tools\"", "id = \"Dev.Example\"")]),
            with(&[("id = \"dev.example.tools\"", "id = \"example\"")]),
            with(&[("id = \"dev.example.tools\"", &long)]),
            with(&[(
                "id = \"dev.example.tools.bun-cache\"",
                "id = \"other.pack.bun-cache\"",
            )]),
            with(&[(
                "id = \"dev.example.tools.bun-cache\"",
                "id = \"dev.example.tools.Bun\"",
            )]),
        ],
    );
}

#[test]
fn the_same_rule_id_twice_is_refused() {
    let rule = format!("[[rule]]{}", VALID.split("[[rule]]").nth(1).unwrap());

    refused(ErrorCode::DuplicateId, &[format!("{VALID}\n{rule}")]);
}

#[test]
fn versions_must_be_semver_and_accept_this_engine() {
    refused(
        ErrorCode::InvalidVersion,
        &[
            with(&[("version = \"1.2.0\"", "version = \"1.2\"")]),
            with(&[("kipple = \">=0.0.0\"", "kipple = \"newest\"")]),
        ],
    );
    refused(
        ErrorCode::EngineIncompatible,
        &[with(&[("kipple = \">=0.0.0\"", "kipple = \">=99.0.0\"")])],
    );
}

#[test]
fn a_rule_may_only_narrow_its_packs_platforms() {
    refused(
        ErrorCode::PlatformMismatch,
        &[with(&[(
            "category = \"cache\"",
            "category = \"cache\"\nplatforms = [\"windows\"]",
        )])],
    );
}

#[test]
fn a_pack_holds_at_most_64_rules() {
    let (head, rule) = VALID.split_once("[[rule]]").unwrap();
    let rules = (0..65)
        .map(|n| rule.replace("bun-cache\"", &format!("bun-cache-{n}\"")))
        .collect::<Vec<_>>()
        .join("[[rule]]");

    refused(ErrorCode::TooManyRules, &[format!("{head}[[rule]]{rules}")]);
}

#[test]
fn a_rule_holds_at_most_32_predicates() {
    let predicates = (0..33)
        .map(|n| format!("f{n} = true\n"))
        .collect::<Vec<_>>()
        .concat();

    refused(
        ErrorCode::TooManyPredicates,
        &[with(&[(
            "[rule.eligibility]\n",
            &format!("[rule.eligibility]\n{predicates}"),
        )])],
    );
}

#[test]
fn patterns_and_names_outside_the_grammar_are_refused() {
    let pattern = |p: &str| with(&[(".bun/install/cache", p)]);
    let nine = ["a"; 9].join("/");
    let long = format!("{}/{}/b", "a".repeat(64), "a".repeat(64));
    let artifact = |marker: &str, artifact: &str| {
        with(&[
            ("root = \"home\"", "root = \"config.project-roots\""),
            (
                "selection = \"dirs-matching\"\npattern = \".bun/install/cache\"",
                &format!(
                    "selection = \"project-artifact\"\nmarker = [\"{marker}\"]\nartifact = \"{artifact}\""
                ),
            ),
        ])
    };
    refused(
        ErrorCode::InvalidPattern,
        &[
            pattern("/etc/cache"),
            pattern(".bun/../cache"),
            pattern(".bun/./cache"),
            pattern(".bun//cache"),
            pattern(".bun/cache/"),
            pattern("C:/cache"),
            pattern("\\\\\\\\server\\\\share"),
            pattern(".bun/cache:stream"),
            pattern(".bun/ca\\u0000che"),
            pattern(".bun/**/cache"),
            pattern(&nine),
            pattern(&long),
            pattern(&format!(".bun/{}", "c".repeat(65))),
            artifact("*.toml", "target"),
            artifact("Cargo.toml", "a/b"),
            artifact("Cargo.toml", ".."),
        ],
    );
}

#[test]
fn a_home_rule_must_start_its_pattern_with_a_literal_name() {
    refused(
        ErrorCode::InvalidPattern,
        &[
            with(&[(".bun/install/cache", "*/cache")]),
            with(&[(".bun/install/cache", ".../cache")]),
            with(&[(
                "selection = \"dirs-matching\"\npattern = \".bun/install/cache\"",
                "selection = \"project-artifact\"\nmarker = [\"package.json\"]\nartifact = \"node_modules\"",
            )]),
        ],
    );
    assert!(
        check(
            &with(&[(".bun/install/cache", ".cache/*/tmp")]),
            Origin::ThirdParty
        )
        .is_ok(),
        "a home pattern starting with a literal name is accepted"
    );
}

#[test]
fn parameters_and_bounds_out_of_range_are_refused() {
    let project = |params: &str| {
        with(&[
            ("root = \"home\"", "root = \"config.project-roots\""),
            (
                "selection = \"dirs-matching\"\npattern = \".bun/install/cache\"",
                &format!("selection = \"project-artifact\"\nartifact = \"target\"\n{params}"),
            ),
        ])
    };
    let markers = (0..9)
        .map(|n| format!("\"m{n}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let references = ["\"https://example.com\""; 6].join(", ");
    refused(
        ErrorCode::InvalidRange,
        &[
            with(&[("pattern = ", "max_matches = 0\npattern = ")]),
            with(&[("pattern = ", "max_matches = 10001\npattern = ")]),
            project("marker = [\"Cargo.toml\"]\nmax_depth = 13"),
            project("marker = []"),
            project(&format!("marker = [{markers}]")),
            with(&[("recent_days = 14", "recent_days = 366")]),
            with(&[("{ min = \"30d\" }", "{ min = \"2y\", max = \"1y\" }")]),
            with(&[("{ min = \"30d\" }", "{}")]),
            with(&[("{ min = \"30d\" }", "{ max = \"101y\" }")]),
            with(&[(
                "tracked_files_in_item = \"absent\"",
                "tracked_files_in_item = { max = 1000001 }",
            )]),
            with(&[(
                "[\"https://bun.sh/docs/install/cache\"]",
                &format!("[{references}]"),
            )]),
        ],
    );
}

#[test]
fn capabilities_this_engine_does_not_publish_are_refused() {
    refused(
        ErrorCode::UnknownCapability,
        &[
            with(&[("root = \"home\"", "root = \"codex.release-store\"")]),
            with(&[(
                "selection = \"dirs-matching\"",
                "selection = \"everything\"",
            )]),
            with(&[("activity = [\"idle\"]", "colour = \"blue\"")]),
            with(&[("activity = [\"idle\"]", "marker.present = true")]),
            with(&[(
                "kind = \"remove-files\"\nmethods = [\"delete\", \"trash\"]",
                "kind = \"remove-verified-release\"",
            )]),
        ],
    );
    let builtin = |edits: &[(&str, &str)]| {
        let mut all = vec![
            ("dev.example.tools", "builtin.example"),
            ("dev.example.tools", "builtin.example"),
        ];
        all.extend_from_slice(edits);
        with(&all)
    };
    for text in [
        builtin(&[("integration = \"generic\"", "integration = \"codex\"")]),
        builtin(&[
            ("integration = \"generic\"", "integration = \"cargo\""),
            (
                "selection = \"dirs-matching\"\npattern = \".bun/install/cache\"",
                "selection = \"bytecode-caches\"",
            ),
        ]),
    ] {
        assert_eq!(
            codes(&text, Origin::Builtin),
            [ErrorCode::UnknownCapability],
            "\n{text}"
        );
    }
}

#[test]
fn third_party_packs_may_not_use_first_party_capabilities() {
    refused(
        ErrorCode::NotPermitted,
        &[
            with(&[("dev.example.tools", "builtin.example")]),
            with(&[("integration = \"generic\"", "integration = \"python\"")]),
            with(&[(
                "selection = \"dirs-matching\"\npattern = \".bun/install/cache\"",
                "selection = \"bytecode-caches\"",
            )]),
        ],
    );
}

#[test]
fn a_builtin_pack_may_use_its_integrations_selectors() {
    let text = with(&[
        ("dev.example.tools", "builtin.example"),
        ("dev.example.tools", "builtin.example"),
        ("integration = \"generic\"", "integration = \"python\""),
        ("root = \"home\"", "root = \"config.project-roots\""),
        (
            "selection = \"dirs-matching\"\npattern = \".bun/install/cache\"",
            "selection = \"bytecode-caches\"",
        ),
    ]);

    let pack = check(&text, Origin::Builtin).unwrap();

    assert_eq!(
        pack.rules[0].selector,
        Selector::Integration("bytecode-caches")
    );
}
