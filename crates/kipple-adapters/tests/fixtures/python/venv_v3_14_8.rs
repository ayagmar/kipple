//! A Python 3.14.8 project with a `python -m venv .venv` environment and the bytecode
//! cache of an imported module, and such an environment on its own, under any name.

use super::tree::FixtureTree;

/// A project at `at`.
pub(crate) fn project(tree: &mut FixtureTree, at: &str) {
    tree.file(
        &format!("{at}/pyproject.toml"),
        b"[project]\nname = \"tool\"\n",
    )
    .file(&format!("{at}/pkg/mod.py"), b"VALUE = 1\n")
    .sized(
        &format!("{at}/pkg/__pycache__/mod.cpython-314.pyc"),
        2 * 1024,
    );
    environment(tree, &format!("{at}/.venv"));
}

/// A `python -m venv` environment at `at`, with a package and its bytecode installed.
pub(crate) fn environment(tree: &mut FixtureTree, at: &str) {
    tree.file(
        &format!("{at}/pyvenv.cfg"),
        b"home = /usr/bin\ninclude-system-site-packages = false\nversion = 3.14.8\n",
    )
    .file(&format!("{at}/.gitignore"), b"*\n")
    .file(&format!("{at}/bin/activate"), b"# activate\n")
    .dir(&format!("{at}/include"))
    .sized(
        &format!("{at}/lib/python3.14/site-packages/pip/__init__.py"),
        1024 * 1024,
    )
    .sized(
        &format!("{at}/lib/python3.14/site-packages/pip/__pycache__/__init__.cpython-314.pyc"),
        512 * 1024,
    );
}
