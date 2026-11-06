//! Reading a golden: a directory of `<integration>.txt` files, one row a line, so that
//! work on one integration edits only that integration's files.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

/// The rows of each file in `tests/golden/<kind>`, by integration.
pub(crate) fn rows(kind: &str) -> io::Result<BTreeMap<String, Vec<String>>> {
    rows_in(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(kind),
    )
}

/// The rows of each file in `dir`, by integration. A file not named `.txt` is an error: it
/// could otherwise take the key of the file it sits beside.
pub(crate) fn rows_in(dir: &Path) -> io::Result<BTreeMap<String, Vec<String>>> {
    fs::read_dir(dir)?
        .map(|entry| {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(integration) = name.strip_suffix(".txt") else {
                let stray = format!("{name} in a golden isn't named `.txt`");
                return Err(io::Error::new(io::ErrorKind::InvalidData, stray));
            };
            let text = fs::read_to_string(entry.path())?;
            let rows = text.lines().map(str::to_owned).collect();
            Ok((integration.to_owned(), rows))
        })
        .collect()
}
