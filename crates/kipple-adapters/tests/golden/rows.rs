//! Reading a golden: a directory of `<integration>.txt` files, one row a line, so that
//! work on one integration edits only that integration's files.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

/// The rows of each file in `tests/golden/<kind>`, by integration. A file not named
/// `.txt` keeps its whole name, so it matches no integration and fails the test.
pub(crate) fn rows(kind: &str) -> io::Result<BTreeMap<String, Vec<String>>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(kind);
    fs::read_dir(dir)?
        .map(|entry| {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let text = fs::read_to_string(entry.path())?;
            let integration = name.strip_suffix(".txt").unwrap_or(&name).to_owned();
            Ok((integration, text.lines().map(str::to_owned).collect()))
        })
        .collect()
}
