//! systemd 262's persistent journal: one directory per machine ID, holding the active
//! and archived journal files.

use super::tree::FixtureTree;

/// A journal at `at`.
pub(crate) fn journal(tree: &mut FixtureTree, at: &str) {
    let machine = format!("{at}/faa3f00a5d3a4acb97b6b964ade1c349");
    tree.sized(&format!("{machine}/system.journal"), 16 * 1024 * 1024)
        .sized(
            &format!("{machine}/system@d2dab7188f4b48ba907c8fee13d5f50c-0000000000000001-00064cfe77cdc550.journal"),
            32 * 1024 * 1024,
        );
}
