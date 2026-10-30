//! `cargo xtask check-style`: the style rules of `docs/08-engineering-standards.md` §4 that
//! clippy can't check.
//!
//! It parses every Rust source with `syn`, so it sees items, not lines. It doesn't look
//! inside macro calls or `macro_rules!` bodies.

use std::collections::BTreeSet;
use std::io::{self, Write as _};
use std::mem;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use syn::visit::{self, Visit};
use syn::{
    Fields, FnArg, GenericArgument, Generics, PathArguments, ReturnType, Signature, Type,
    TypeParamBound, Visibility,
};

use crate::arch::{self, SourceFile};

/// The directories whose Rust sources are checked.
const SOURCES: &[&str] = &["crates", "xtask"];

/// An item allowed to break one rule, and why.
#[derive(Debug)]
struct Exemption {
    /// The file, relative to the workspace root.
    file: &'static str,
    /// What the check reports, without the file: the item, then the rule it breaks.
    found: &'static str,
    reason: &'static str,
}

/// The items allowed to break a rule. Keep it short: fix the item instead when you can.
const EXEMPT: &[Exemption] = &[Exemption {
    file: "crates/kipple-platform/src/environment.rs",
    found: "impl `Environment`, fn `from_vars`: takes a tuple",
    reason: "takes `std::env::vars_os()` as it is, which gives each variable as a tuple",
}];

/// Fails if a Rust source in the workspace breaks a style rule.
pub(crate) fn run(root: &Path) -> Result<()> {
    let violations = violations(&arch::rust_sources(root, SOURCES)?, EXEMPT)?;
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

/// Every style violation in `files` that `exempt` doesn't allow, sorted, and every
/// exemption that no longer allows anything, so the list can't go stale.
fn violations(files: &[SourceFile], exempt: &[Exemption]) -> Result<Vec<String>> {
    let mut violations = Vec::new();
    let mut used = BTreeSet::new();
    for file in files {
        for found in file_violations(file)? {
            let allowed = exempt.iter().position(|e| {
                file.path == Path::new(e.file) && found.starts_with(&format!("{}; ", e.found))
            });
            match allowed {
                Some(index) => {
                    used.insert(index);
                }
                None => violations.push(format!("{}: {found}", file.path.display())),
            }
        }
    }
    for (index, unused) in exempt.iter().enumerate() {
        if !used.contains(&index) {
            violations.push(format!(
                "{}: {}: exempt ({}), but no longer found; remove the exemption",
                unused.file, unused.found, unused.reason
            ));
        }
    }
    violations.sort();
    Ok(violations)
}

/// The style violations in one file, each with the item it is in.
fn file_violations(file: &SourceFile) -> Result<Vec<String>> {
    let ast = syn::parse_file(&file.contents)
        .with_context(|| format!("could not parse {}", file.path.display()))?;
    let mut style = Style {
        trait_impl: false,
        item: String::new(),
        found: Vec::new(),
    };
    style.visit_file(&ast);
    Ok(style.found)
}

/// Walks one file and remembers where it is.
struct Style {
    /// Inside a trait's `impl`, whose signatures the trait decides.
    trait_impl: bool,
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

    /// A signature outside a trait `impl`, visible outside its module, that takes or
    /// returns a tuple.
    fn check_signature(&mut self, vis: &Visibility, sig: &Signature) {
        if !is_visible(vis) || self.trait_impl {
            return;
        }
        let takes = sig.inputs.iter().any(|input| match input {
            FnArg::Typed(arg) => has_tuple(&arg.ty),
            FnArg::Receiver(_) => false,
        });
        if takes {
            self.report("takes a tuple; take a struct with named fields");
        }
        if returns_tuple(&sig.output) {
            self.report("returns a tuple; return a struct with named fields");
        }
    }

    /// Fields visible outside their module whose type holds a tuple. A variant's fields
    /// are as visible as its enum, `enum_vis`.
    fn check_fields(&mut self, fields: &Fields, enum_vis: Option<&Visibility>) {
        for (index, field) in fields.iter().enumerate() {
            let vis = enum_vis.unwrap_or(&field.vis);
            if !is_visible(vis) || !has_tuple(&field.ty) {
                continue;
            }
            let name = field
                .ident
                .as_ref()
                .map_or_else(|| index.to_string(), ToString::to_string);
            self.report(&format!(
                "field `{name}` holds a tuple; use a struct with named fields"
            ));
        }
    }
}

impl<'ast> Visit<'ast> for Style {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        self.within(format!("mod `{}`", node.ident), |style| {
            visit::visit_item_mod(style, node);
        });
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.within(format!("fn `{}`", node.sig.ident), |style| {
            style.check_signature(&node.vis, &node.sig);
            visit::visit_item_fn(style, node);
        });
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        let outer = mem::replace(&mut self.trait_impl, node.trait_.is_some());
        let item = format!("impl `{}`", type_name(&node.self_ty));
        self.within(item, |style| visit::visit_item_impl(style, node));
        self.trait_impl = outer;
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        let item = format!("{}, fn `{}`", self.item, node.sig.ident);
        self.within(item, |style| {
            style.check_signature(&node.vis, &node.sig);
            visit::visit_impl_item_fn(style, node);
        });
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        self.within(format!("trait `{}`", node.ident), |style| {
            let methods = node.items.iter().filter_map(|item| match item {
                syn::TraitItem::Fn(method) => Some(method),
                syn::TraitItem::Const(_)
                | syn::TraitItem::Type(_)
                | syn::TraitItem::Macro(_)
                | syn::TraitItem::Verbatim(_)
                | _ => None,
            });
            for method in methods {
                style.check_signature(&node.vis, &method.sig);
            }
            visit::visit_item_trait(style, node);
        });
    }

    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        self.within(format!("struct `{}`", node.ident), |style| {
            style.check_lifetimes(&node.generics);
            style.check_fields(&node.fields, None);
            visit::visit_item_struct(style, node);
        });
    }

    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        self.within(format!("enum `{}`", node.ident), |style| {
            style.check_lifetimes(&node.generics);
            for variant in &node.variants {
                style.check_fields(&variant.fields, Some(&node.vis));
            }
            visit::visit_item_enum(style, node);
        });
    }

    fn visit_item_union(&mut self, node: &'ast syn::ItemUnion) {
        self.within(format!("union `{}`", node.ident), |style| {
            style.check_lifetimes(&node.generics);
            visit::visit_item_union(style, node);
        });
    }

    fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
        self.within(format!("type `{}`", node.ident), |style| {
            if is_visible(&node.vis) && has_tuple(&node.ty) {
                style.report("holds a tuple; use a struct with named fields");
            }
            visit::visit_item_type(style, node);
        });
    }
}

/// Whether `vis` lets other modules see an item: anything but private and `pub(self)`.
fn is_visible(vis: &Visibility) -> bool {
    match vis {
        Visibility::Inherited => false,
        Visibility::Restricted(restricted) => !restricted.path.is_ident("self"),
        Visibility::Public(_) => true,
    }
}

/// Whether `ty` is or holds a tuple other than `()`, such as `Vec<(u64, u64)>` or
/// `impl Iterator<Item = (Name, Path)>`.
fn has_tuple(ty: &Type) -> bool {
    match ty {
        Type::Tuple(tuple) => !tuple.elems.is_empty(),
        Type::Array(array) => has_tuple(&array.elem),
        Type::Slice(slice) => has_tuple(&slice.elem),
        Type::Reference(reference) => has_tuple(&reference.elem),
        Type::Ptr(ptr) => has_tuple(&ptr.elem),
        Type::Paren(paren) => has_tuple(&paren.elem),
        Type::Group(group) => has_tuple(&group.elem),
        Type::Path(path) => path_has_tuple(&path.path),
        Type::ImplTrait(bounds) => bounds.bounds.iter().any(bound_has_tuple),
        Type::TraitObject(bounds) => bounds.bounds.iter().any(bound_has_tuple),
        Type::FnPtr(fn_ptr) => {
            fn_ptr.inputs.iter().any(|arg| has_tuple(&arg.ty)) || returns_tuple(&fn_ptr.output)
        }
        Type::Infer(_) | Type::Macro(_) | Type::Never(_) | Type::Verbatim(_) | _ => false,
    }
}

fn returns_tuple(output: &ReturnType) -> bool {
    match output {
        ReturnType::Type(_, ty) => has_tuple(ty),
        ReturnType::Default => false,
    }
}

fn bound_has_tuple(bound: &TypeParamBound) -> bool {
    match bound {
        TypeParamBound::Trait(bound) => path_has_tuple(&bound.path),
        TypeParamBound::Lifetime(_)
        | TypeParamBound::PreciseCapture(_)
        | TypeParamBound::Verbatim(_)
        | _ => false,
    }
}

/// Whether a path's generic arguments hold a tuple: `Option<(u64, u64)>`, `Fn((A, B))`.
fn path_has_tuple(path: &syn::Path) -> bool {
    path.segments
        .iter()
        .any(|segment| match &segment.arguments {
            PathArguments::AngleBracketed(args) => args.args.iter().any(|arg| match arg {
                GenericArgument::Type(ty) => has_tuple(ty),
                GenericArgument::AssocType(assoc) => has_tuple(&assoc.ty),
                GenericArgument::Lifetime(_)
                | GenericArgument::Const(_)
                | GenericArgument::AssocConst(_)
                | GenericArgument::Constraint(_)
                | _ => false,
            }),
            PathArguments::Parenthesized(args) => {
                args.inputs.iter().any(|arg| has_tuple(&arg.ty)) || returns_tuple(&args.output)
            }
            PathArguments::None => false,
        })
}

/// The last name in a type's path, for the report.
fn type_name(ty: &Type) -> String {
    let Type::Path(path) = ty else {
        return String::from("_");
    };
    path.path
        .segments
        .last()
        .map_or_else(String::new, |s| s.ident.to_string())
}
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    const FILE: &str = "crates/kipple/src/a.rs";

    fn source(contents: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from(FILE),
            contents: contents.to_owned(),
        }
    }

    fn check(contents: &str) -> Vec<String> {
        violations(&[source(contents)], &[]).unwrap()
    }

    // Catches a struct or enum whose `'a` says nothing about what it borrows, so a
    // reader can't tell two borrows apart or what outlives what.
    #[test]
    fn a_one_letter_lifetime_on_a_type_fails() {
        let found = check(
            "struct Tree<'a> { lines: &'a [Line] }\nenum Frame<'p> { Chroot(&'p Path) }\n\
             mod inner { pub(crate) union Raw<'b> { bytes: &'b [u8] } }\n",
        );
        assert_eq!(
            found,
            [
                "crates/kipple/src/a.rs: enum `Frame`: lifetime `'p`; name it for what the type borrows",
                "crates/kipple/src/a.rs: struct `Tree`: lifetime `'a`; name it for what the type borrows",
                "crates/kipple/src/a.rs: union `Raw`: lifetime `'b`; name it for what the type borrows",
            ]
        );
    }

    // Catches the check reaching past types: `'a` is fine on functions and impls, and a
    // named lifetime passes.
    #[test]
    fn named_type_lifetimes_and_one_letter_fn_lifetimes_pass() {
        let found = check(
            "struct Tree<'lines> { lines: &'lines [Line] }\n\
             impl<'a> Tree<'a> { fn first<'b>(&'b self) -> &'a Line { &self.lines[0] } }\n\
             fn longest<'a>(x: &'a str) -> &'a str { x }\n",
        );
        assert_eq!(found, Vec::<String>::new());
    }

    // Catches a tuple in a signature or field other modules see, where two values of
    // one type can be swapped without a compile error: `(line, column)` returned, a pair
    // taken, a tuple nested in a `Vec` or an iterator, a field and a variant.
    #[test]
    fn a_tuple_in_a_visible_signature_or_field_fails() {
        let found = check(
            "pub(crate) fn position(text: &str) -> Option<(usize, usize)> { None }\n\
             pub fn join(hints: &[(String, String)]) {}\n\
             impl Roots { pub fn iter(&self) -> impl Iterator<Item = (Root, &Path)> { todo!() } }\n\
             pub(super) struct Error { pub(super) position: (usize, usize) }\n\
             pub enum Event { Moved(Vec<(u64, u64)>) }\n\
             pub trait Sized { fn size(&self) -> (u64, u64); }\n\
             pub type Pair = (u64, u64);\n",
        );
        assert_eq!(found.len(), 7, "{found:#?}");
        assert!(
            found.contains(
                &"crates/kipple/src/a.rs: fn `position`: returns a tuple; return a struct with named fields"
                    .to_owned()
            ),
            "a returned tuple is reported at its function: {found:#?}"
        );
        assert!(
            found.contains(
                &"crates/kipple/src/a.rs: impl `Roots`, fn `iter`: returns a tuple; return a struct with named fields"
                    .to_owned()
            ),
            "a method is reported with its type: {found:#?}"
        );
    }

    // Catches the check failing what the rule allows: private functions and fields, a
    // trait impl whose signature the trait decides, `()`, a newtype and a struct.
    #[test]
    fn private_tuples_trait_impls_unit_and_newtypes_pass() {
        let found = check(
            "fn split(path: &str) -> (String, String) { todo!() }\n\
             pub(self) fn pair() -> (u8, u8) { (0, 0) }\n\
             struct Walk { linked: Vec<(Identity, Linked)> }\n\
             pub struct Id(pub u64);\n\
             pub fn check() -> Result<(), Error> { Ok(()) }\n\
             impl Iterator for Lines { type Item = (usize, String); fn next(&mut self) -> Option<(usize, String)> { None } }\n\
             pub struct Place { pub line: usize, pub column: usize }\n",
        );
        assert_eq!(found, Vec::<String>::new());
    }

    // Catches an exemption that hides more than the one item and rule it names, and one
    // left behind after its item is fixed, which would let the next violation in silently.
    #[test]
    fn an_exemption_allows_its_one_item_and_rule_and_fails_once_unused() {
        let exempt = [Exemption {
            file: FILE,
            found: "fn `from_vars`: takes a tuple",
            reason: "std gives tuples",
        }];
        let file = source(
            "pub fn from_vars(vars: Vec<(String, String)>) -> (u8, u8) { (0, 0) }\n\
             pub fn from_pairs(vars: Vec<(String, String)>) {}\n",
        );
        let found = violations(&[file], &exempt).unwrap();
        assert_eq!(
            found,
            [
                "crates/kipple/src/a.rs: fn `from_pairs`: takes a tuple; take a struct with named fields",
                "crates/kipple/src/a.rs: fn `from_vars`: returns a tuple; return a struct with named fields",
            ]
        );

        let fixed = source("pub fn from_vars(vars: Vars) {}\n");
        let stale = violations(&[fixed], &exempt).unwrap();
        assert_eq!(stale.len(), 1, "{stale:?}");
        assert!(
            stale[0].ends_with("but no longer found; remove the exemption"),
            "a stale exemption fails: {stale:?}"
        );
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
