//! Cursor: S5 Cursor §7 and pinned 36. Nothing of it is verified, so it opens nothing and its
//! roots only protect.

use kipple_core::{Hold, IntegrationId, OwnerStatus, Platform, Proposal, Protection};

use super::host::{Host, Scan};
use super::tree::FixtureTree;

const CURSOR: IntegrationId = IntegrationId("cursor");
const GIT: IntegrationId = IntegrationId("git");
const APP_DATA: &str = "home/.config/Cursor";
const RULE: &str = "builtin.cursor.app-data-size";

/// What Cursor leaves on a machine, as far as upstream's docs and forum say.
fn cursor() -> FixtureTree {
    let mut tree = FixtureTree::new();
    tree.file(&format!("{APP_DATA}/User/settings.json"), b"{}\n")
        .file(
            &format!("{APP_DATA}/User/globalStorage/state.vscdb"),
            b"SQLite format 3\0",
        )
        .file(&format!("{APP_DATA}/Cache/data_0"), b"\0")
        .file(
            "home/.cursor/sdk/auth.json",
            b"{\"token\":\"credential-value\"}\n",
        )
        .file("home/.cursor/worktrees/repo/name/README.md", b"# Repo\n");
    tree
}

fn on(platform: Platform) -> Scan {
    let mut host = Host::new(cursor());
    host.platform = platform;
    host.scan()
}

/// Catches a third-party rule reaching a cache only a forum post describes: the whole
/// directory is closed, whoever asks.
#[test]
fn third_party_rule_cannot_reach_cursor_cache() {
    let scan = on(Platform::Linux);

    for asker in [IntegrationId::GENERIC, GIT, CURSOR] {
        let cache = format!("{APP_DATA}/Cache/data_0");
        assert_eq!(
            scan.protection(&cache, asker),
            Some(Protection::Subresource)
        );
    }
}

/// Catches user data or credentials becoming a candidate on any OS: Windows reports the
/// directory whole and proposes nothing, and nothing inside it is read.
#[test]
fn cursor_user_data_never_a_candidate() {
    let linux = on(Platform::Linux);
    assert_eq!(linux.all(CURSOR), [""; 0]);

    let windows = on(Platform::Windows);
    assert_eq!(windows.found(RULE), [APP_DATA]);
    assert_eq!(windows.proposal(APP_DATA), Proposal::Advice);
    assert_eq!(windows.holds(APP_DATA), [Hold::Advice]);
    assert_eq!(windows.owner(CURSOR), OwnerStatus::Unsupported);
    for closed in [
        format!("{APP_DATA}/User/settings.json"),
        "home/.cursor/sdk/auth.json".to_owned(),
    ] {
        assert_eq!(
            windows.protection(&closed, CURSOR),
            Some(Protection::Subresource)
        );
    }
    assert_eq!(windows.read(), [""; 0]);
}

/// Catches a worktree under `~/.cursor` turning removable because Git found it: with an
/// unverified owner there's no open area, whatever S5's table says (pinned 8, 36).
#[test]
fn cursor_worktree_is_reported_never_removed() {
    let scan = on(Platform::Linux);

    let worktree = "home/.cursor/worktrees/repo/name";
    assert_eq!(
        scan.protection(worktree, GIT),
        Some(Protection::Subresource)
    );
}

/// Catches advice appearing, or an omission, on a machine without Cursor.
#[test]
fn absent_cursor_reports_nothing() {
    let scan = Host::new(FixtureTree::new()).scan();

    assert_eq!(scan.all(CURSOR), [""; 0]);
    assert_eq!(scan.advice(CURSOR).len(), 0, "expected no advice");
    assert_eq!(scan.owner(CURSOR), OwnerStatus::Unsupported);
}

/// Catches advice that states a default upstream doesn't: the interval's 6 is an example, and
/// the cleanup reaches worktrees kipple would call a person's own.
#[test]
fn advice_states_the_upstream_cleanup() {
    let scan = on(Platform::Linux);

    let advice = scan.advice(CURSOR);
    assert_eq!(advice.len(), 1, "expected one advice");
    let text = &advice[0].text;
    assert!(
        text.contains("25 by default"),
        "expected the count's default in {text}"
    );
    assert!(
        !text.contains("6 hours") && !text.contains("Hours (6"),
        "expected no default for the interval in {text}"
    );
    assert!(
        text.contains("plain git worktree add"),
        "expected the warning in {text}"
    );
}
