# Engineering standards

Status: steps 1–14, §2–§4 and §8 are in place and enforced. The `native` nextest profile and `cargo xtask test-native` (§5) exist since M1, and so does `cargo xtask verify-demo`, the end-to-end check of the CLI with the `test-roots` feature, which CI runs beside the gate on all three OSes. Later steps are planned.

## 1. One gate

`cargo xtask check` is the only definition of green. This list is authoritative, and other docs link here.

| # | Step | Command run by xtask | Active from |
| ---: | --- | --- | --- |
| 1 | Format | `cargo fmt --all --check` | M0 |
| 2 | Lint | `cargo clippy --workspace --all-targets --locked -- -D warnings`, then the same with `--all-features`: the first lints the build that ships, the second the code behind `test-roots` | M0 |
| 3 | Rustdoc | `cargo doc --workspace --no-deps --locked` with `RUSTDOCFLAGS=-D warnings` | M0 |
| 4 | Tests | `cargo nextest run --workspace --locked` and `cargo test --doc --workspace --locked` | M0 |
| 5 | Supply chain | `cargo deny check` (CI runs it as a separate job; see 10-ci-and-release.md §1) | M0 |
| 6 | Unused deps | `cargo machete` | M0 |
| 7 | Spelling | `typos` | M0 |
| 8 | Architecture | `cargo xtask check-arch` (see §3) | M0 |
| 9 | Generated CLI reference | `cargo xtask gen-docs --check` for the commands that exist | M0 (only `--version` and `--help`) |
| 10 | Generated JSON schemas and support matrix | part of `gen-docs --check` | M1 |
| 11 | Compatibility with the last release | `cargo xtask check-compat` (see §8) | M2 |
| 12 | Documented commands | `cargo xtask check-docs`: every `kipple` command in a shell block of `README.md`, `docs/` and `site/src/` parses against the CLI (see the rules below) | M2 |
| 13 | Style | `cargo xtask check-style`: the rules of §4 that clippy can't check | M2 |
| 14 | Workflow and shell lint | `actionlint` (which runs `shellcheck` on every `run:` block), `zizmor` and `shellcheck` on `.githooks/` and `*.sh`, in CI's `lint` job only; xtask doesn't run them | M2 |
| 15 | Site build, links, tapes, Lighthouse | `cargo xtask site` (separate CI job) | M6 |

Rules:
- A step that is not yet active doesn't exist in the code or in CI. It isn't a skipped or silently passing step. Each milestone adds its steps in the same change that introduces their inputs.
- `gen-docs` generates only what the binary actually has. It never references future commands.
- A fenced `sh`, `shell`, `bash` or `zsh` block, and each `$ ` line of a `console` block, shows commands that work today, and `check-docs` parses every `kipple` command in them with the CLI's own clap definition, without running anything. It finds the command after assignments, a `$ ` prompt, `env`, `sudo`, `if`, `then`, `do` and the like, inside `$(…)` and backticks (quoted or not), and as a path such as `./target/release/kipple`. A line that names `kipple` any other way, such as `nice kipple scan`, fails with "can't tell how this runs `kipple`", except as an argument of `echo` or `printf`. Planned commands, such as the full CLI surface in 03 §3, go in `text` blocks, which it skips, as it skips inline code. It fails when it finds no command at all, so it can't pass by reading nothing.
- A milestone is done when `cargo xtask check` is green on all three OSes in CI, plus the milestone's own verification commands in [11-delivery-plan.md](11-delivery-plan.md).
- A pre-commit hook in `.githooks/` runs the fast subset (fmt, clippy, typos).
- We never loosen a gate to get a change through. Changing a gate is its own change, with a stated reason.

## 2. Lints

Workspace-level `[workspace.lints]`, the same baseline as the founder's other Rust work (niri-computer-use):

- `unsafe_code = "forbid"` in every crate except `kipple-platform`, which uses `deny` and allows reviewed `#[expect(unsafe_code, reason = "...")]` items only where a syscall has no safe wrapper. Prefer `rustix` or `cap-std` first.
- Clippy `all = deny`, `pedantic`, `nursery` and `cargo = warn`, and `-D warnings` in CI turns them all into errors.
- Denied: `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `unwrap_in_result`, `panic_in_result_fn`, `exit`, `mem_forget`, `dbg_macro`, `print_stdout`, `print_stderr`, `allow_attributes`, `allow_attributes_without_reason`, `let_underscore_must_use`.
- Warned: `cognitive_complexity` (allow-by-default, so it is enabled explicitly; it is a rough signal, not a measure of understandability), `indexing_slicing`, `string_slice`, `wildcard_enum_match_arm`, `shadow_unrelated`, `clone_on_ref_ptr`, `mod_module_files`, `str_to_string`, `unreachable_pub`, `missing_debug_implementations`.
- `#[allow]` is banned. `#[expect(lint, reason = "...")]` is used only when the lint is wrong for that one item.

`clippy.toml`:

```toml
cognitive-complexity-threshold = 15
too-many-lines-threshold = 80
too-many-arguments-threshold = 5
excessive-nesting-threshold = 4
max-fn-params-bools = 0
allow-unwrap-in-tests = true
allow-expect-in-tests = true
allow-indexing-slicing-in-tests = true
allow-panic-in-tests = true
disallowed-methods = [
  { path = "std::fs::remove_file",      reason = "mutation goes through kipple-platform's confined executor" },
  { path = "std::fs::remove_dir",       reason = "mutation goes through kipple-platform's confined executor" },
  { path = "std::fs::remove_dir_all",   reason = "mutation goes through kipple-platform's confined executor" },
  { path = "std::fs::rename",           reason = "mutation goes through kipple-platform's confined executor" },
  # The raw calls the executor removes with, so its call sites carry the same expectation.
  # Only kipple-platform links libc and windows-sys, hence `allow-invalid` elsewhere.
  { path = "libc::unlinkat",            reason = "mutation goes through kipple-platform's confined executor", allow-invalid = true },
  { path = "windows_sys::Win32::Storage::FileSystem::SetFileInformationByHandle", reason = "mutation goes through kipple-platform's confined executor", allow-invalid = true },
  { path = "std::process::Command::new", reason = "native commands go through the platform runner (identity, deadline, env)" },
  { path = "std::env::var",             reason = "read environment once in the composition root and inject it" },
  { path = "std::env::set_var",         reason = "process-global mutation" },
  { path = "std::thread::sleep",        reason = "use the injected Clock or a cancellable wait" },
]
```

The runner and each OS's executor carry one `#[expect(clippy::disallowed_methods, reason = ...)]` at the single call site that is allowed. Grep for that expectation and you have the complete mutation surface.

Limits are a smell detector, not a target. Split code by responsibility, never just to get under a number. If a limit is wrong for one function, use `#[expect]` with a reason.

## 3. Architecture checks

`cargo xtask check-arch` reads `cargo metadata` and fails on:
- any kipple crate that core depends on
- `kipple-adapters` and `kipple-platform` depending on each other
- `#[cfg(target_os` outside `kipple-platform`
- a module of the binary other than `human` wording a core enum that `human` words, or one worded in two frontend modules ([05-architecture.md](05-architecture.md) §4)

These are cheap and deterministic, and they protect the boundaries in [05-architecture.md](05-architecture.md). We add no custom "smell score".

## 4. Code style

- Newtypes over comments: IDs, `ByteCount`, `Allocated` vs `Apparent`, `ResolvedRoot`, `PathRef`. Units can't be mixed by accident.
- Internal items are `pub(crate)`. A crate's public API is the contract the GUI will use, so keep it small.
- Errors use `thiserror`, carry structured context and keep the upstream detail (OS error, exit code, stderr). `anyhow` appears only in `xtask`.
- No global state, no lazy statics holding configuration, no `std::env` reads below the composition root.
- Pure functions for policy, planning, retention, rendering and TUI state transitions. I/O happens at the edges.
- Early returns over nested conditionals. No dead code, commented-out code or stale TODOs.

- No `bool` parameter: `run(root, true)` doesn't say what `true` means. Take a two-variant enum named for what it means there, such as `compat::run(root, Baselines::Keep)`, or split the function in two. Clippy checks it: `max-fn-params-bools = 0` makes `fn_params_excessive_bools` fail on any function with one, so it can't tell a literal call site from one that passes a named value. A trait `impl` is exempt, since the trait decides its signature.

These rules are checked by `cargo xtask check-style` (gate step 13), which parses every Rust source in `crates/` and `xtask/` with `syn`. It doesn't look inside `macro_rules!` bodies or macro calls other than `assert!`.

- No tuple in a signature or field other modules see: a function's parameters and return type, a trait method's, a field, an enum variant's field or a type alias, when it is `pub`, `pub(crate)`, `pub(super)` or `pub(in ...)`. That counts a tuple nested in a type, such as `Vec<(u64, u64)>` or `impl Iterator<Item = (Root, &Path)>`. Use a struct with named fields, so two values of one type can't be swapped. `()`, newtypes, private items and trait `impl`s, whose signatures the trait decides, are fine.
- A struct's, enum's or union's lifetime is named for what it borrows, such as `Tree<'lines>`; a one-letter name fails. `'a` is fine on functions and `impl` blocks.
- In test code (a file under `tests/`, a `tests.rs`, a `#[cfg(test)]` item or a `#[test]` function), an `assert!` or `debug_assert!` with only a condition gets a message that says what state it expects, such as `assert!(found.complete, "the search completes")`. Without one, a failure prints the expression but not why it should hold or what it was. `==` and `!=` are exempt, since clippy's `manual_assert_eq` already turns them into `assert_eq!` and `assert_ne!`, which print both sides. Every other condition (`<`, `matches!`, a method call, a negation) needs the message. Asserts outside test code state invariants and aren't checked.

An item may break a rule only through an entry in the check's `EXEMPT` list, which names the item and the rule and says why. An entry that no longer matches anything fails too.

## 5. Testing philosophy

Write tests that would catch a real bug a user would notice. Don't write tests that only restate the code.

Test cases come from the spec, not from the implementation. The required failure scenarios (04 §10), the integration fixtures (06 §7) and each milestone's verify lines (11) are the human-written statement of intent. Tests an agent invents from its own code only confirm its own reading of the task. Before adding a test that isn't traceable to the spec, name the realistic bug it would catch.

**Do test:**
- Policy and eligibility decisions from fact sets, including the unknown-blocks cases
- Retention: keep-active, keep-running, keep-N, per-group
- Plan revalidation that only subtracts
- Integration layouts against `FixtureTree` fixtures, one per supported upstream version (06 §7)
- Confinement, idempotence and dedup as seeded property tests (a generated case per seed, so a failure names the seed that reproduces it), since those are the invariants that matter
- Manifest parsing and path normalization with `cargo-fuzz` (nightly CI job, not the gate)
- CLI end-to-end against the shared `demo-home` with every root redirected by the `test-roots` override, plus `insta` snapshots of the JSON report (`cargo xtask verify-demo`)
- TUI state transitions, plus buffer snapshots at 80×24 and 160×48
- Each platform executor and trash backend on its native OS, with disposable directories
- The default path, not only edge cases

**Don't test:**
- Getters, `From` impls, derives, or that clap parses a flag clap already tested
- Mocks that just return what the test told them to
- A unit test per private helper. Test the behaviour through the module's API
- Coverage percentage. There is no coverage gate

**Isolation.** Tests never read or write the real `$HOME`, real tool stores, the user's trash, the external network, or the user's git config and hooks. The one network exception is a separately marked integration test that talks to a disposable loopback HTTPS server, trusting a test-only CA in that client only. Setting environment variables alone is not enough:
- **Roots are injected.** Platform root resolution (XDG, Known Folders, `~/Library`, and on Linux `/etc/pacman.conf` and `/var` through the environment's system root) sits behind a port, and ordinary tests pass fixture roots directly. No test depends on `APPDATA` redirection changing what Windows Known Folder APIs return.
- **Git** runs with an empty, disposable config file (`GIT_CONFIG_GLOBAL=<tempdir>/gitconfig`, `GIT_CONFIG_NOSYSTEM=1`, `core.hooksPath` set to an empty temp dir), on fixture repos created inside the test.
- **Native backends** (OS trash on macOS and Windows, Known Folders, process probes) touch real OS state that can't be redirected. They live in test targets named `native`, which only the `native` nextest profile in `.config/nextest.toml` runs; the default profile excludes them. They run only on disposable hosts: CI's GitHub runner VMs, or a local throwaway VM or OS account. Locally, `cargo xtask test-native` refuses to run unless `KIPPLE_TEST_DISPOSABLE_HOST=1` is set, and the docs say to set it only inside such a VM. On Linux, the freedesktop trash tests use a disposable `XDG_DATA_HOME` and run in the default profile.
- **Sentinels.** Native and end-to-end tests place sentinel files only inside their disposable boundary (the temp root, or the disposable host's home). They assert that only the planned targets changed.
- Time comes from an injected `Clock`.

Fake-path tests prove logic. Only native-profile tests count as platform evidence for the release gates in [04-safety-model.md](04-safety-model.md) §10.

## 6. Dependencies

- Newest stable release that is at least 7 days old, for every crate, tool, action and toolchain. Record the publish date and the reason in `docs/decisions.md` when the dependency is added.
- Each dependency must earn its place. Ask whether it is maintained, what its transitive weight is, and whether the license is on the `deny.toml` allowlist. Prefer std where it is good enough.
- Release binary size is tracked in CI. A jump of more than 10% needs a note in the PR.
- No async runtime in the library crates.

## 7. Commits and reviews

- Conventional Commits, `type(scope): summary`, imperative, lowercase, at most 72 characters. No AI attribution, `Co-authored-by` lines or emoji.
- Each milestone ends with an independent read-only review (a separate agent in its own pane) that runs until the verdict is READY.
- Docs change in the same commit as the behaviour they describe.

## 8. Compatibility across versions

Data one version of kipple writes or reads must be read by the next. Each format kipple publishes or reads has a **baseline**, the contract of the last release, in `crates/kipple/tests/compat/<format>/<version>/`. There has been no release yet, so today's baselines are the first contract, version 1 of each format.

What a baseline holds:
- `schema.json`, for a format with a JSON schema: the `kipple scan --json`, `scan --events jsonl`, `rules --json` and `doctor --json` records, the support matrix, and the theme file.
- Sample documents, for a format kipple reads: a `config.toml`, theme files and a third-party rule pack, between them setting every key the release reads. Beside a document, a `.expected` file lists lines `kipple config show` or `kipple theme show` must print for it.

**The schema check.** `cargo xtask check-compat` (gate step 11) compares each published schema with its baseline. A compatible change depends on who reads the documents:
- **Records kipple writes** (`scan`, `event`, `rules`, `doctor`, `support-matrix`) are read by scripts. A consumer of kipple's JSON may rely on every property, type, value and variant the baseline lists, and must ignore properties it doesn't know. So a new property, optional or always present, is compatible. These break it: removing or renaming a property, a property the baseline always writes becoming optional or nullable, and any type, value or variant the baseline doesn't allow, such as a new bucket or event type. A value the schema leaves a free string, such as a hold reason or an omission kind, is outside the check.
- **Documents kipple reads** (`theme`, and from M3 the journal, receipts and profiles) are written by people or an earlier kipple. Every document the baseline allows must still be read. A new value, a wider type, or a new optional property where the baseline refuses properties it doesn't list is compatible. These break it: removing or renaming a property, a new required property, a narrower type or enum, a tighter bound or pattern, refusing properties the baseline left open, and a new property that refuses a value the baseline left open, such as a `note` that must now be a string.

The check fails, rather than passes, on a schema keyword it doesn't compare anywhere in either schema, new properties included, and on a published schema that `FORMATS` in `xtask/src/compat.rs` doesn't list. `pattern` and `format` are compared for equality. A document matching two variants of a `oneOf` matches none, so the check fails on a `oneOf` unless it can show no two variants share a document: their constants, enum values or types differ, a string `pattern` starts with other text, or a property both require can't take the same value. `--refresh` writes a first baseline only from a schema the check can judge.

**The reader tests.** `crates/kipple/tests/compat.rs` reads every sample document of every version the baselines hold, so a document written for any earlier release still loads: the config with `config show` printing each line of its `.expected` file, each theme with `theme show` doing the same, and each pack validating as a third-party pack. They need the `test-roots` feature, so they run with the other end-to-end tests in `cargo xtask verify-demo`. When only the wording of `show` changes, update the `.expected` file in the same commit and say so.

**A breaking change** bumps the format's version (`kipple.scan/v2`, `version = 2` in a theme) and needs a baseline for it: `cargo xtask check-compat --refresh` writes the schema of a version that has none. A reader keeps reading every older version it has a baseline for. A config file has no version, so a change that breaks a baseline config isn't allowed.

**Refreshing the baseline.** At each release, `cargo xtask check-compat --refresh` makes each compatible schema its baseline, so what the release added becomes part of the contract (10-ci-and-release.md §3, Flow). Add sample documents for any key the release reads that the samples don't set yet. `--refresh` never replaces a baseline the current schema breaks.

**Adding a format** is one baseline directory and one test: an entry in `FORMATS` and `--refresh` for a JSON schema, and for a format kipple reads, its sample documents and a test in `compat.rs` that reads them through kipple's public interface.

