//! npm 11.20.0: its install as a release tarball or `npm install -g npm@11.20.0` lays it out,
//! and the cache it writes. Its `package.json` is upstream's at tag `v11.20.0`, cut to the
//! fields kipple reads (`name`, `version`, `engines`, `bin`). cacache 20.0.4 writes `content-v2/`, `index-v5/` and `tmp/`, and no `CACHEDIR.TAG` (S5 npm §1, §3).

use super::tree::FixtureTree;

/// The version.
pub(crate) const VERSION: &str = "11.20.0";

/// The node versions its `package.json` accepts.
pub(crate) const ENGINES: &str = "^20.17.0 || >=22.9.0";

/// A node inside [`ENGINES`], as S5 ran it with.
pub(crate) const NODE: &str = "26.12.1";

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
}
