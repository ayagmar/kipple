//! npm 12.2.1: its install as a release tarball or `npm install -g npm@12.2.1` lays it out,
//! and the cache it writes. Its `package.json` is upstream's at tag `v12.2.1`, cut to the
//! fields kipple reads (`name`, `version`, `engines`, `bin`). cacache 21 writes the same `content-v2/` and `index-v5/`, and `CACHEDIR.TAG` (S5 npm §1, §3).

use super::tree::FixtureTree;

/// The version.
pub(crate) const VERSION: &str = "12.2.1";

/// The node versions its `package.json` accepts.
pub(crate) const ENGINES: &str = "^22.22.2 || ^24.15.0 || >=26.0.0";

/// A node inside [`ENGINES`], as S5 ran it with.
pub(crate) const NODE: &str = "26.13.0";

/// The install below `prefix`: `lib/node_modules/npm`, with its `bin/npm-cli.js`, and
/// `bin/npm` linking to it. The node beside it, `bin/node`, is the test's to make.
pub(crate) fn install(tree: &mut FixtureTree, prefix: &str) {
    let package = format!("{prefix}/lib/node_modules/npm");
    let manifest = format!(
        "{{\"name\":\"npm\",\"version\":\"{VERSION}\",\"engines\":{{\"node\":\"{ENGINES}\"}},\"bin\":{{\"npm\":\"bin/npm-cli.js\",\"npx\":\"bin/npx-cli.js\"}}}}"
    );
    tree.file(&format!("{package}/package.json"), manifest.as_bytes())
        .file(
            &format!("{package}/bin/npm-cli.js"),
            b"#!/usr/bin/env node\nrequire('../lib/cli.js')(process)\n",
        )
        .file(
            &format!("{package}/bin/npx-cli.js"),
            b"#!/usr/bin/env node\n",
        )
        .link(
            &format!("{prefix}/bin/npm"),
            "../lib/node_modules/npm/bin/npm-cli.js",
            super::tree::Holds::AsWritten,
        );
}

/// A cache root at `root` as this version leaves it after an install and an `npx` run:
/// `_cacache` with one entry, `_logs`, `_npx` holding an installed package, and the
/// update notifier's marker.
pub(crate) fn cache(tree: &mut FixtureTree, root: &str) {
    let cacache = format!("{root}/_cacache");
    tree.sized(
        &format!("{cacache}/content-v2/sha512/9b/4e/7c0ffee0a1b2c3d4e5f60718293a4b5c6d7e8f9"),
        64 * 1024,
    )
    .file(
        &format!("{cacache}/index-v5/1f/2e/3d4c5b6a79881726354433221100ffeeddccbbaa"),
        b"\n5d41402abc4b2a76b9719d911017c592\t{\"key\":\"make-fetch-happen:request-cache:https://registry.npmjs.org/dep/-/dep-1.0.0.tgz\"}\n",
    )
    .dir(&format!("{cacache}/tmp"))
    .file(
        &format!("{root}/_logs/2026-11-06T10_00_00_000Z-debug-0.log"),
        b"0 verbose cli\n",
    )
    .file(
        &format!("{root}/_npx/0123456789abcdef/package.json"),
        b"{\"dependencies\":{\"tool\":\"1.0.0\"}}",
    )
    .file(
        &format!("{root}/_npx/0123456789abcdef/package-lock.json"),
        b"{\"lockfileVersion\":3}",
    )
    .file(
        &format!("{root}/_npx/0123456789abcdef/node_modules/.package-lock.json"),
        b"{\"lockfileVersion\":3}",
    )
    .file(
        &format!("{root}/_npx/0123456789abcdef/node_modules/tool/package.json"),
        b"{\"name\":\"tool\"}",
    )
    .file(&format!("{root}/_update-notifier-last-checked"), b"");
    tree.file(
        &format!("{root}/_cacache/CACHEDIR.TAG"),
        b"Signature: 8a477f597d28d172789f06886806bc55\n",
    );
}
