//! The trash, pacman's package cache and the journal: each reported whole, as the root
//! itself, with native retention advice where the tool has its own (`docs/06-adapters-
//! and-rules.md` §5 and §6). Emptying or pruning them needs the tool or root, so kipple
//! only reports.

use std::path::Path;

use kipple_core::{
    Advice, Evidence, FindingSink, Integration, IntegrationDescriptor, IntegrationError,
    IntegrationId, IntegrationSelector, Lens, Platform, ProbeError, ScanContext, Selector,
};

/// Whether the root at a path holds anything worth reporting.
type HoldsAnything = fn(&ScanContext<'_>, &Path) -> Result<bool, ProbeError>;

/// Adds each path of the root of every rule using `selector`, when it holds anything.
/// Returns whether any of those roots opened, which is when advice applies.
fn add_roots(
    cx: &ScanContext<'_>,
    sink: &mut FindingSink<'_>,
    selector: &'static str,
    holds_anything: HoldsAnything,
) -> Result<bool, IntegrationError> {
    let mut opened = false;
    let rules = cx
        .rules()
        .iter()
        .filter(|rule| rule.selector == Selector::Integration(selector));
    for rule in rules {
        for root in cx.roots_of(rule.root) {
            opened = true;
            if !holds_anything(cx, &root.path)? {
                continue;
            }
            sink.add(rule, &root.path, vec![Evidence::Root(rule.root)])?;
        }
    }
    Ok(opened)
}

/// Whether the directory at `path` has any entry.
fn not_empty(cx: &ScanContext<'_>, path: &Path) -> Result<bool, ProbeError> {
    Ok(!cx.read_dir(path)?.is_empty())
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|&item| item.to_owned()).collect()
}

/// The current user's trash: freedesktop's home trash on Linux, `~/.Trash` on macOS.
#[derive(Debug)]
pub(crate) struct Trash;

const TRASH_CONTENTS: &str = "trash-contents";

impl Integration for Trash {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            id: IntegrationId("trash"),
            lens: Lens::System,
            platforms: &[Platform::Linux, Platform::Macos],
            roots: &[],
            selectors: &[IntegrationSelector {
                name: TRASH_CONTENTS,
                meaning: "the trash itself, when it holds anything",
            }],
            artifacts: &[],
            owner_files: &[],
            verified: &["freedesktop.org Trash specification 1.0"],
        }
    }

    fn discover(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        add_roots(cx, sink, TRASH_CONTENTS, not_empty).map(drop)
    }
}

/// pacman's package cache.
#[derive(Debug)]
pub(crate) struct Pacman;

const PACKAGE_CACHE: &str = "package-cache";

impl Integration for Pacman {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            id: IntegrationId("pacman"),
            lens: Lens::System,
            platforms: &[Platform::Linux],
            roots: &[],
            selectors: &[IntegrationSelector {
                name: PACKAGE_CACHE,
                meaning: "pacman's package cache itself, when it holds anything",
            }],
            artifacts: &[],
            owner_files: &[],
            verified: &["pacman 7.1.0"],
        }
    }

    fn discover(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        if add_roots(cx, sink, PACKAGE_CACHE, not_empty)? {
            sink.advise(Advice {
                title: "Keep fewer old packages with paccache".to_owned(),
                text: "paccache, from pacman-contrib, removes cached packages except the \
                       newest versions of each. Keeping three lets you roll back a bad \
                       upgrade without the network; keeping fewer saves more space but \
                       leaves fewer versions to downgrade to. paccache.timer runs it every \
                       week. kipple never runs it: it needs root."
                    .to_owned(),
                snippets: strings(&[
                    "sudo paccache -rk3",
                    "sudo systemctl enable --now paccache.timer",
                ]),
                references: strings(&[
                    "https://wiki.archlinux.org/title/Pacman#Cleaning_the_package_cache",
                ]),
            });
        }
        Ok(())
    }
}

/// systemd-journald's persistent journal.
#[derive(Debug)]
pub(crate) struct Journald;

const JOURNAL_FILES: &str = "journal-files";

impl Integration for Journald {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            id: IntegrationId("journald"),
            lens: Lens::System,
            platforms: &[Platform::Linux],
            roots: &[],
            selectors: &[IntegrationSelector {
                name: JOURNAL_FILES,
                meaning: "the persistent journal itself, when it holds anything",
            }],
            artifacts: &[],
            owner_files: &[],
            verified: &["systemd 262"],
        }
    }

    fn discover(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        if add_roots(cx, sink, JOURNAL_FILES, not_empty)? {
            sink.advise(Advice {
                title: "Cap the journal's size".to_owned(),
                text: "journalctl --vacuum-size removes the oldest archived journal files \
                       until the archived ones fit the size. It leaves the active files alone, \
                       so the journal can stay larger; --rotate first archives them too. \
                       SystemMaxUse= in a journald.conf drop-in caps the journal from then \
                       on. Both delete old log entries. kipple never runs them: they need \
                       root."
                    .to_owned(),
                snippets: strings(&[
                    "sudo journalctl --rotate --vacuum-size=500M",
                    "# /etc/systemd/journald.conf.d/size.conf\n[Journal]\nSystemMaxUse=500M",
                ]),
                references: strings(&[
                    "https://www.freedesktop.org/software/systemd/man/latest/journald.conf.html",
                    "https://www.freedesktop.org/software/systemd/man/latest/journalctl.html",
                ]),
            });
        }
        Ok(())
    }
}
