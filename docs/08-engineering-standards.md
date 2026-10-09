# Engineering standards

Status: planned. These configs are created in M0 and enforced from the first commit of code.

## 1. One gate

`cargo xtask check` is the only definition of green. This list is authoritative, and other docs link here.

| # | Step | Command run by xtask | Active from |
| ---: | --- | --- | --- |
| 1 | Format | `cargo fmt --all --check` | M0 |
| 2 | Lint | `cargo clippy --workspace --all-targets --locked -- -D warnings` | M0 |
| 3 | Rustdoc | `cargo doc --workspace --no-deps --locked` with `RUSTDOCFLAGS=-D warnings` | M0 |
| 4 | Tests | `cargo nextest run --workspace --locked` and `cargo test --doc --workspace --locked` | M0 |
| 5 | Supply chain | `cargo deny check` (CI runs it as a separate job; see 10-ci-and-release.md §1) | M0 |
| 6 | Unused deps | `cargo machete` | M0 |
| 7 | Spelling | `typos` | M0 |
| 8 | Architecture | `cargo xtask check-arch` (see §3) | M0 |
| 9 | Generated CLI reference | `cargo xtask gen-docs --check` for the commands that exist | M0 (only `--version` and `--help`) |
| 10 | Generated JSON schemas and support matrix | part of `gen-docs --check` | M1 |
| 11 | Site build, links, tapes, Lighthouse | `cargo xtask site` (separate CI job) | M6 |

Rules:
- A step that is not yet active doesn't exist in the code or in CI. It isn't a skipped or silently passing step. Each milestone adds its steps in the same change that introduces their inputs.
- `gen-docs` generates only what the binary actually has. It never references future commands.
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
allow-unwrap-in-tests = true
allow-expect-in-tests = true
allow-indexing-slicing-in-tests = true
allow-panic-in-tests = true
disallowed-methods = [
  { path = "std::fs::remove_file",      reason = "mutation goes through kipple-platform's confined executor" },
  { path = "std::fs::remove_dir",       reason = "mutation goes through kipple-platform's confined executor" },
  { path = "std::fs::remove_dir_all",   reason = "mutation goes through kipple-platform's confined executor" },
  { path = "std::fs::rename",           reason = "mutation goes through kipple-platform's confined executor" },
  { path = "std::process::Command::new", reason = "native commands go through the platform runner (identity, deadline, env)" },
  { path = "std::env::var",             reason = "read environment once in the composition root and inject it" },
  { path = "std::env::set_var",         reason = "process-global mutation" },
  { path = "std::thread::sleep",        reason = "use the injected Clock or a cancellable wait" },
]
```

The executor and the runner each carry one `#[expect(clippy::disallowed_methods, reason = ...)]` at the single call site that is allowed. Grep for that expectation and you have the complete mutation surface.

Limits are a smell detector, not a target. Split code by responsibility, never just to get under a number. If a limit is wrong for one function, use `#[expect]` with a reason.

## 3. Architecture checks

`cargo xtask check-arch` reads `cargo metadata` and fails on:
- any kipple crate that core depends on
- `kipple-adapters` and `kipple-platform` depending on each other
- `#[cfg(target_os` outside `kipple-platform`

These are cheap and deterministic, and they protect the boundaries in [05-architecture.md](05-architecture.md). We add no custom "smell score".

## 4. Code style

- Newtypes over comments: IDs, `ByteCount`, `Allocated` vs `Apparent`, `ResolvedRoot`, `PathRef`. Units can't be mixed by accident.
- Internal items are `pub(crate)`. A crate's public API is the contract the GUI will use, so keep it small.
- Errors use `thiserror`, carry structured context and keep the upstream detail (OS error, exit code, stderr). `anyhow` appears only in `xtask`.
- No global state, no lazy statics holding configuration, no `std::env` reads below the composition root.
- Pure functions for policy, planning, retention, rendering and TUI state transitions. I/O happens at the edges.
- Early returns over nested conditionals. No dead code, commented-out code or stale TODOs.

## 5. Testing philosophy

Write tests that would catch a real bug a user would notice. Don't write tests that only restate the code.

**Do test:**
- Policy and eligibility decisions from fact sets, including the unknown-blocks cases
- Retention: keep-active, keep-running, keep-N, per-group
- Plan revalidation that only subtracts
- Integration layouts against `FixtureTree` fixtures, one per supported upstream version (06 §7)
- Confinement, idempotence and dedup with `proptest`, since those are the invariants that matter
- Manifest parsing and path normalization with `cargo-fuzz` (nightly CI job, not the gate)
- CLI end-to-end against fixture homes with `assert_cmd` plus `insta` snapshots of human and JSON output
- TUI state transitions, plus buffer snapshots at 80×24 and 160×48
- Each platform executor and trash backend on its native OS, with disposable directories
- The default path, not only edge cases

**Don't test:**
- Getters, `From` impls, derives, or that clap parses a flag clap already tested
- Mocks that just return what the test told them to
- A unit test per private helper. Test the behaviour through the module's API
- Coverage percentage. There is no coverage gate

**Isolation.** Tests never read or write the real `$HOME`, real tool stores, the user's trash, the external network, or the user's git config and hooks. The one network exception is a separately marked integration test that talks to a disposable loopback HTTPS server, trusting a test-only CA in that client only. Setting environment variables alone is not enough:
- **Roots are injected.** Platform root resolution (XDG, Known Folders, `~/Library`) sits behind a port, and ordinary tests pass fixture roots directly. No test depends on `APPDATA` redirection changing what Windows Known Folder APIs return.
- **Git** runs with an empty, disposable config file (`GIT_CONFIG_GLOBAL=<tempdir>/gitconfig`, `GIT_CONFIG_NOSYSTEM=1`, `core.hooksPath` set to an empty temp dir), on fixture repos created inside the test.
- **Native backends** (OS trash on macOS and Windows, Known Folders, process probes) touch real OS state that can't be redirected. They live in a separate nextest profile, `native`, which the default profile excludes. They run only on disposable hosts: CI's GitHub runner VMs, or a local throwaway VM or OS account. Locally, `cargo xtask test-native` refuses to run unless `KIPPLE_TEST_DISPOSABLE_HOST=1` is set, and the docs say to set it only inside such a VM. On Linux, the freedesktop trash tests use a disposable `XDG_DATA_HOME` and run in the default profile.
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
