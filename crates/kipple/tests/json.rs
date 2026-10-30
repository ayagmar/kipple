//! JSON paths are lossless (`docs/03-functional-spec.md` §4): a path that isn't valid
//! Unicode keeps its exact bytes, so automation can still name the file.

use kipple::json::PathOut;

#[cfg(unix)]
#[test]
fn a_path_that_is_not_utf8_keeps_its_exact_bytes_in_raw() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Path;

    let path = Path::new(OsStr::from_bytes(b"/tmp/caf\xe9/x"));

    let out = PathOut::new(path);

    assert_eq!(out.display, "/tmp/caf\u{fffd}/x");
    assert_eq!(out.raw.as_deref(), Some("L3RtcC9jYWbpL3g="));
}

#[test]
fn a_unicode_path_needs_no_raw_form() {
    let out = PathOut::new(std::path::Path::new("données/été"));

    assert_eq!(out.display, "données/été");
    assert_eq!(out.raw, None);
}

/// Automation reads the schema to learn what a reason can be, so it names every one.
#[test]
fn the_schema_names_every_reason_a_verdict_can_carry() {
    let schemas = kipple::json::schemas();
    let scan = schemas.iter().find(|s| s.name == "scan").unwrap();
    let described = scan
        .schema
        .as_value()
        .pointer("/$defs/VerdictOut/properties/reasons/description")
        .and_then(|description| description.as_str())
        .unwrap();

    let names = kipple_core::Hold::ALL.iter().map(|hold| hold.name()).chain(
        kipple_core::Caution::ALL
            .iter()
            .map(|caution| caution.name()),
    );
    for name in names {
        assert!(
            described.contains(&format!("`{name}`")),
            "the reasons description lacks `{name}`"
        );
    }
}
