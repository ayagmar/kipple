//! How release versions order: `settle` keeps the newest left when a store names no
//! rollback, so a wrong order removes the only way back.

use kipple_core::ReleaseVersion;

fn version(text: &str) -> ReleaseVersion {
    ReleaseVersion(text.to_owned())
}

fn newest<'text>(versions: &[&'text str]) -> Option<&'text str> {
    versions.iter().copied().max_by_key(|text| version(text))
}

/// Catches a prerelease outranking its release, which would keep the alpha and let the
/// release go.
#[test]
fn a_prerelease_orders_below_its_release() {
    assert_eq!(newest(&["0.166.0", "0.166.0-alpha.2"]), Some("0.166.0"));
    assert_eq!(
        newest(&["0.165.0", "0.166.0-alpha.2"]),
        Some("0.166.0-alpha.2")
    );
}

/// Catches comparing versions as text, where 0.9.0 outranks 0.166.0.
#[test]
fn versions_order_by_number_not_by_text() {
    assert_eq!(newest(&["0.9.0", "0.166.0"]), Some("0.166.0"));
    assert_eq!(
        newest(&["1.0.0-alpha.9", "1.0.0-alpha.10"]),
        Some("1.0.0-alpha.10")
    );
    assert_eq!(
        newest(&["1.0.0-alpha", "1.0.0-alpha.1"]),
        Some("1.0.0-alpha.1")
    );
    assert_eq!(newest(&["1.0.0-1", "1.0.0-rc"]), Some("1.0.0-rc"));
}

/// Catches a name that isn't a version beating a real one.
#[test]
fn text_that_is_no_version_orders_below_every_version() {
    assert_eq!(newest(&["zzz", "0.0.1"]), Some("0.0.1"));
    assert!(
        version("local-build") != version("local-test"),
        "two names stay distinct when neither is a version"
    );
}
