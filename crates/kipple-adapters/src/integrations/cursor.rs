//! Cursor (`docs/06-adapters-and-rules.md` §5): its descriptor, its roots, all closed whole,
//! and advice on the cleanup it does itself. Nothing of its layout is verified, so it opens
//! nothing, a path that only protects costs nothing when it is wrong, and its one finding is
//! the application data directory on Windows, the one place upstream names it. Its advice was
//! checked against cursor.com's docs on 2026-11-06.

use std::path::Path;

use kipple_core::{
    Advice, AdviceId, Claim, Evidence, FindingSink, Integration, IntegrationDescriptor,
    IntegrationError, IntegrationId, IntegrationSelector, Lens, Platform, Rule, ScanContext,
    Selector, StateDeclaration, Supplied, SymbolicRoot, Unresolved,
};

const APP_DATA: &str = "cursor-app-data";

const SELECTORS: &[IntegrationSelector] = &[IntegrationSelector {
    name: APP_DATA,
    meaning: "the application data directory, as a whole",
}];

const fn closed(root: SymbolicRoot) -> StateDeclaration {
    StateDeclaration {
        root,
        protected: &[],
        open: &[],
        delegates: &[],
        unresolved: Unresolved::HoldOwn,
    }
}

const STATE: &[StateDeclaration] = &[
    closed(SymbolicRoot::CursorConfig),
    closed(SymbolicRoot::CursorAppData),
    closed(SymbolicRoot::CursorCli),
    closed(SymbolicRoot::CursorServer),
];

/// The Cursor integration.
#[derive(Debug)]
pub(crate) struct Cursor;

impl Integration for Cursor {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            id: IntegrationId("cursor"),
            lens: Lens::Agents,
            platforms: Platform::ALL,
            roots: &[SymbolicRoot::CursorConfig, SymbolicRoot::CursorAppData],
            selectors: SELECTORS,
            artifacts: &[],
            owner_files: &[],
            verified: &[],
            consequences: &[],
            facts: &[],
            state: STATE,
            evidences: &[],
            revision: 2,
        }
    }

    /// Advises wherever Cursor has left a directory. Whether one is there is all it asks of
    /// them: nothing inside is read, and it never says its layout was recognised, so every
    /// root stays closed.
    fn discover(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        if cx.roots().is_empty() {
            return Ok(());
        }
        let advice = sink.advise(advice());
        let rules = cx
            .rules()
            .iter()
            .filter(|rule| rule.selector == Selector::Integration(APP_DATA));
        for rule in rules {
            for root in cx.roots_of(rule.root) {
                add_whole(cx, sink, rule, &root.path, advice)?;
            }
        }
        Ok(())
    }
}

/// Reports the directory at `path` as a whole, when it holds anything.
fn add_whole(
    cx: &ScanContext<'_>,
    sink: &mut FindingSink<'_>,
    rule: &Rule,
    path: &Path,
    advice: AdviceId,
) -> Result<(), IntegrationError> {
    if cx.read_dir(path)?.is_empty() {
        return Ok(());
    }
    let claim = Claim {
        evidence: vec![Evidence::Root(rule.root)],
        consequence: None,
    };
    let supplied = Supplied {
        advice: Some(advice),
        ..sink.supplied()
    };
    sink.add_supplied(rule, path, claim, supplied)
}

fn advice() -> Advice {
    Advice {
        title: "Cursor's own cleanup".to_owned(),
        text: "Old Agent chats collect in a local database that can reach many gigabytes. Run \
               Delete Old Chats…, then GC Agent KV Blobs, which is what shrinks the file. \
               Cursor also removes its own worktrees: cursor.worktreeMaxCount keeps the newest \
               ones up to a count across every workspace (25 by default), \
               cursor.worktreeCleanupIntervalHours sets how often that runs, and Max Total \
               Size caps them at 50 GB by default. That cleanup also covers a worktree made \
               with a plain git worktree add under its directory, so work there can go without \
               kipple. kipple only reports Cursor's worktrees, and doesn't read its settings."
            .to_owned(),
        snippets: vec![
            "{ \"cursor.worktreeMaxCount\": 25, \"cursor.worktreeCleanupIntervalHours\": 6 }"
                .to_owned(),
        ],
        references: vec![
            "https://cursor.com/docs/configuration/worktrees.md".to_owned(),
            "https://cursor.com/help/troubleshooting/install-issues.md".to_owned(),
        ],
    }
}
