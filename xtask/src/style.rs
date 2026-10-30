//! `cargo xtask check-style`: the style rules of `docs/08-engineering-standards.md` §4 that
//! clippy can't check.
//!
//! It parses every Rust source with `syn`, so it sees items, not lines. It doesn't look
//! inside macro calls or `macro_rules!` bodies.

use std::io::{self, Write as _};
use std::mem;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use syn::Generics;
use syn::visit::{self, Visit};

use crate::arch::{self, SourceFile};

/// The directories whose Rust sources are checked.
const SOURCES: &[&str] = &["crates", "xtask"];

/// Fails if a Rust source in the workspace breaks a style rule.
pub(crate) fn run(root: &Path) -> Result<()> {
    let mut violations = Vec::new();
    for file in arch::rust_sources(root, SOURCES)? {
        violations.extend(file_violations(&file)?);
    }
    violations.sort();
    if violations.is_empty() {
        return Ok(());
    }
    let mut stderr = io::stderr().lock();
    for violation in &violations {
        writeln!(stderr, "check-style: {violation}")?;
    }
    bail!(
        "{} style violation(s), see docs/08-engineering-standards.md §4",
        violations.len()
    )
}

/// The style violations in one file, each with its path and the item it is in.
fn file_violations(file: &SourceFile) -> Result<Vec<String>> {
    let ast = syn::parse_file(&file.contents)
        .with_context(|| format!("could not parse {}", file.path.display()))?;
    let mut style = Style {
        item: String::new(),
        found: Vec::new(),
    };
    style.visit_file(&ast);
    Ok(style
        .found
        .into_iter()
        .map(|v| format!("{}: {v}", file.path.display()))
        .collect())
}

/// Walks one file and remembers where it is.
struct Style {
    /// The innermost named item, which the report names since `syn` keeps no lines here.
    item: String,
    found: Vec<String>,
}

impl Style {
    /// Visits an item's insides as `item`.
    fn within(&mut self, item: String, visit: impl FnOnce(&mut Self)) {
        let outer_item = mem::replace(&mut self.item, item);
        visit(self);
        self.item = outer_item;
    }

    fn report(&mut self, message: &str) {
        self.found.push(format!("{}: {message}", self.item));
    }

    /// A struct's, enum's or union's lifetime with a one-letter name, which says nothing
    /// about what it borrows.
    fn check_lifetimes(&mut self, generics: &Generics) {
        for param in generics.lifetimes() {
            let name = param.lifetime.ident.to_string();
            if name.chars().count() == 1 {
                self.report(&format!(
                    "lifetime `'{name}`; name it for what the type borrows"
                ));
            }
        }
    }
}

impl<'ast> Visit<'ast> for Style {
    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        self.within(format!("struct `{}`", node.ident), |style| {
            style.check_lifetimes(&node.generics);
            visit::visit_item_struct(style, node);
        });
    }

    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        self.within(format!("enum `{}`", node.ident), |style| {
            style.check_lifetimes(&node.generics);
            visit::visit_item_enum(style, node);
        });
    }

    fn visit_item_union(&mut self, node: &'ast syn::ItemUnion) {
        self.within(format!("union `{}`", node.ident), |style| {
            style.check_lifetimes(&node.generics);
            visit::visit_item_union(style, node);
        });
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn violations(path: &str, contents: &str) -> Vec<String> {
        file_violations(&SourceFile {
            path: PathBuf::from(path),
            contents: contents.to_owned(),
        })
        .unwrap()
    }

    // Catches a struct or enum whose `'a` says nothing about what it borrows, so a
    // reader can't tell two borrows apart or what outlives what.
    #[test]
    fn a_one_letter_lifetime_on_a_type_fails() {
        let found = violations(
            "crates/kipple/src/a.rs",
            "struct Tree<'a> { lines: &'a [Line] }\nenum Frame<'p> { Chroot(&'p Path) }\n\
             mod inner { pub(crate) union Raw<'b> { bytes: &'b [u8] } }\n",
        );
        assert_eq!(
            found,
            [
                "crates/kipple/src/a.rs: struct `Tree`: lifetime `'a`; name it for what the type borrows",
                "crates/kipple/src/a.rs: enum `Frame`: lifetime `'p`; name it for what the type borrows",
                "crates/kipple/src/a.rs: union `Raw`: lifetime `'b`; name it for what the type borrows",
            ]
        );
    }

    // Catches the check reaching past types: `'a` is fine on functions and impls, and a
    // named lifetime passes.
    #[test]
    fn named_type_lifetimes_and_one_letter_fn_lifetimes_pass() {
        let found = violations(
            "crates/kipple/src/a.rs",
            "struct Tree<'lines> { lines: &'lines [Line] }\n\
             impl<'a> Tree<'a> { fn first<'b>(&'b self) -> &'a Line { &self.lines[0] } }\n\
             fn longest<'a>(x: &'a str) -> &'a str { x }\n",
        );
        assert_eq!(found, Vec::<String>::new());
    }

    // Catches the check passing by reading nothing: it reads every crate and xtask.
    #[test]
    fn the_sources_are_every_crate_and_xtask() {
        let files = arch::rust_sources(Path::new(crate::WORKSPACE_ROOT), SOURCES).unwrap();
        let has = |path: &str| files.iter().any(|f| f.path == Path::new(path));
        assert!(
            has("crates/kipple-core/src/lib.rs"),
            "the sources include the crates"
        );
        assert!(has("xtask/src/style.rs"), "the sources include xtask");
    }
}
