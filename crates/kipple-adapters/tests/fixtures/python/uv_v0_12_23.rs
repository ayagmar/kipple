//! A uv 0.12.23 project: `uv venv` writes `pyvenv.cfg` and a cache directory tag.

use super::tree::FixtureTree;

/// A project at `at`.
pub(crate) fn project(tree: &mut FixtureTree, at: &str) {
    tree.file(
        &format!("{at}/pyproject.toml"),
        b"[project]\nname = \"uvproj\"\n",
    )
    .file(&format!("{at}/uv.lock"), b"version = 1\n")
    .file(
        &format!("{at}/.venv/pyvenv.cfg"),
        b"home = /usr/bin\nimplementation = CPython\nuv = 0.12.23\nversion_info = 3.14.8\n",
    )
    .file(
        &format!("{at}/.venv/CACHEDIR.TAG"),
        b"Signature: 8a477f597d28d172789f06886806bc55",
    )
    .file(&format!("{at}/.venv/.gitignore"), b"*\n")
    .sized(
        &format!("{at}/.venv/lib/python3.14/site-packages/uvproj.pth"),
        64,
    );
}
