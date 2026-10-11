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
