# Delivery plan

Status: planned. A milestone is done when all of these hold:
1. `cargo xtask check` is green on Linux, macOS and Windows CI, with the gate steps active for that milestone ([08-engineering-standards.md](08-engineering-standards.md) §1).
2. The milestone's verification commands below give the stated results.
3. An independent read-only review (a separate agent in its own pane) reaches READY.
4. A handoff note exists for the next fresh session.

**Stop conditions for every milestone.** Stop and ask the founder instead of improvising when:
- a spike result contradicts a decision in [decisions.md](decisions.md)
- an upstream layout cannot be verified
- a safety invariant in [04-safety-model.md](04-safety-model.md) can't be met on an OS

The fallback is never to weaken a check. It is to ship that piece as report-only.

**Live machines.** Acceptance always runs on fixtures and disposable CI VMs. A run on the founder's own machine is an optional manual smoke test with these rules:
- it starts read-only (`kipple scan`)
- its baseline is recorded first
- anything that mutates needs the founder's explicit go-ahead for the exact plan, every time

It never replaces fixture evidence and is never required to finish a milestone.

One exception: a speed budget that assumes more cores and a faster disk than a hosted runner has is measured on the founder's workstation (M2's `bench-tui` figure, M7's budgets). That run works on generated trees in a temp home and reads nothing of the founder's but the process table, so the rules above don't apply to it.

## Spikes (timeboxed, inside the milestone that needs them)

| ID | When | Question | Output |
| --- | --- | --- | --- |
| S1 | M3 | Can `cap-std` (>= 4.0.3) plus std give confined, handle-relative removal on Linux, macOS and Windows: root identity, no link or junction escape, defined behaviour for new children? | Prototype plus `native`-profile tests on all three CI runners. The executor design and its findings go in decisions.md |
| S2 | M1 | Running-executable identity: `sysinfo` vs direct `/proc`, `proc_pidpath` and `QueryFullProcessImageNameW`. Speed, identity (inode or file ID), failure modes. | A decision in decisions.md. Done 2026-10-10: D-035, evidence in decisions.md §4.1 |
| S3 | M1 | Walker and sizing: configured `ignore` vs `dua-core` vs `read_dir` with `rayon`. | Numbers from the procedure below, and a decision. Done 2026-10-10: D-036, evidence in decisions.md §4.2 |
| S4 | M3 | Trash on macOS: `trash` has no list or restore there. Our own receipt-based restore, or "trash only, restore through Finder"? | A decision, and whether `restore` is advertised on macOS |
| S5 | M4 | Upstream layouts and lifecycles for Codex, Claude Code, Pi and Cursor on all three OSes, from docs and open source. | `FixtureTree` fixtures per verified version, plus each integration's fact list |

**S3 procedure.** `cargo xtask bench-tree <dir> --entries 100000|1000000` generates reference trees deterministically (fixed seed, fixed depth and fan-out, with `node_modules`-like hot spots). Each walker variant then runs 5 cold runs (page cache dropped on Linux via a disposable VM) and 5 warm runs. Record median wall time, peak RSS and syscall count (`strace -c` on Linux). A variant is accepted if it is the fastest or within 10% of the fastest without extra dependencies. Results go in decisions.md with the machine description.

## M0: Foundation

Build:
- the Cargo workspace (four crates plus `xtask`), `rust-toolchain.toml` (1.99.0) and `.cargo/config.toml` (xtask alias)
- `[workspace.lints]`, `clippy.toml`, `rustfmt.toml`, `deny.toml`, `typos.toml`
- `.githooks/pre-commit`
- xtask subcommands: `check`, `fix`, `check-arch` and `gen-docs` (the CLI reference for `--help` and `--version` only)
- `.github/workflows/ci.yml` (three OSes, SHA-pinned actions, `deny` job) and `dependabot.yml` (7-day cooldown)

Check all dependency versions again, and record them.

Verify:
- `cargo xtask check` passes locally on Linux, and CI is green on all three OSes.
- `cargo run -p kipple -- --version` prints `kipple 0.0.0`.
- Adding `kipple-platform` as a dependency of `kipple-core` makes `cargo xtask check-arch` fail (try it locally and revert).

## M1: Core model and read-only scan

Status (2026-10-11): complete. Built over three sessions: spikes S2 and S3; the core types, ports and `Engine::scan` with its event contract and the shared project walk (D-044); platform root resolution with sources, the no-follow probes, the D-035 process probe, the D-036 walker and the capability probes; root links (D-041); the schema v1 loader and validator; the read-only Cargo, Node, Python and trash integrations with the pacman, journald and Claude Code advice (06 §5a); `kipple scan`, `rules`, `explain` and `doctor` with `--json` and `--events jsonl`; `cargo xtask demo-home` and `verify-demo` (D-046); and `gen-docs` for the JSON schemas and the support matrix. `dirs-matching` and `files-matching` are validated, and evaluated from M5 when third-party packs arrive (D-044). The S3 budget is recorded in decisions.md §4.3.

Build:
- core types, ports, and `Engine::scan` with the event contract (05 §2)
- platform root resolution and no-follow probes, plus spikes S2 and S3
- the rule loader and validator for schema v1 (06 §3), with `dirs-matching`, `files-matching` and `project-artifact`
- read-only integrations: Cargo `target/`, Node `node_modules`, Python `.venv` and `__pycache__`, trash size
- advice for pacman, journald and Claude Code
- `kipple scan` (human, `--json`, `--events jsonl`), `rules`, `explain`, `doctor`
- platform capability probes (05 §1) and upstream root resolution with source reporting
- `xtask demo-home` and `xtask verify-demo`; turn on `gen-docs` for the JSON schemas and the support matrix

Verify:
- `cargo xtask verify-demo` passes on all three OSes. It builds `demo-home` in a temp dir and runs the built `kipple scan --json` with every root redirected through the hidden test-roots override (`KIPPLE_TEST_ROOTS=<file>`, compiled only with the `test-roots` feature, so release builds can't use it). It then compares the result with the committed `insta` snapshot. No real account store is resolved.
- Validator tests reject every case listed in 06 §3 with the documented error code.
- The S3 procedure is recorded. On the 100k reference tree, known-root findings appear in under 250 ms (warm).
- Tests for a stalled consumer, a dropped consumer and a broken pipe pass.

## M2: TUI home

Status (2026-10-18): complete. Built: bucket classification at Careful, Balanced and Thorough with the facts it needs (D-047) and the 80% check on `demo-home`; the config file (D-051); project activity scoped by declared evidence (D-050); the home screen (D-048) in the bucket layout the founder chose (D-049), with keep (`p`/`P`), the settings screen and the first-run onboarding (J0), their state-transition tests and buffer snapshots at 80×24 and 160×48; `cargo xtask bench-tui` (p95 2.2 ms on the founder's workstation); the `demo-home` snapshots at Careful, Balanced and Thorough; and the vhs tapes for the first scan and navigation, which CI's `tapes` job (09 §7) rendered green in run 38786997494. M2c, the polish the founder asked for after a first look (2026-10-16), added the project roots in the header, project grouping, `o` and size bars (D-054), the suggested roots in onboarding and settings (D-053), the detail pane's scrolling and wording, a project's detail and `y` (D-054).

Build:
- state machine plus a pure renderer: Safe / Review / Protected buckets with Agents / Dev / System grouping, detail pane (what it is, why it's here, why kipple thinks it's safe, what you lose, how it comes back), streaming sizes, filter, selection and the `c` Safe shortcut, help, narrow layout, `NO_COLOR` and `--ascii`
- a lens-first variant behind a dev flag, used only for the M2 comparison and then deleted
- first-run onboarding (J0), carefulness levels, keep (`p`/`P`), and the settings screen
- vhs tapes for first scan and navigation
- M2c, the home screen polish (03 §5, D-054): the project roots in the header, findings grouped by project, `o` for the sort order with the cursor kept on its line, size bars, `y` to copy a path through OSC 52, a detail pane that scrolls with `J`/`K` and says "not estimated" for an unknown restore cost
- M2c, suggested project roots (03 §6, D-053): the bounded read-only search of home, the pure suggestion function in `kipple-core`, and the unchecked suggestions in onboarding step 2 and settings

Verify:
- State-transition tests pass.
- M2c: a project with two or more findings in a group is one collapsible line whose `a` skips recent findings; `o` keeps the cursor on its finding; on `demo-home` the header shows the project roots at 80×24. The suggestion function never suggests home, an existing root, a path inside one or an excluded path, and folds nested suggestions. The search over a `FixtureTree` home stops at its depth, hidden-directory, link, project and budget limits and says when it stopped early. Choosing a suggestion writes `scan.project_roots` once and rescans; quitting writes nothing.
- On `demo-home`, Balanced puts at least 80% of rebuildable, reinstallable and re-downloadable bytes in Safe (04 §2a), among the findings whose activity the OS lets kipple judge. Careful and Thorough match their snapshots.
- Buffer snapshots at 80×24 and 160×48 match.
- Keypress-to-frame p95 is under 50 ms while scanning the 1M reference tree, measured by `xtask bench-tui`.
- The founder compares the bucket and lens layouts on the same fixture data, picks one, and signs off on the feel. The loser is removed. Done: D-049. The sign-off on the feel comes after M2c.

## M3: Safe mutation (two sessions)

Status (2026-10-31): complete. Built: spikes S1 and S4, the confined executor and the trash backend (D-061, D-062); the core's planner, revalidation, apply loop and reconciliation with the frozen-tree budget (D-063, D-064) (M3a); the stored runs with their journal and lock (D-065), the git query apply asks again before each item (D-066), plan files that are planned again from a fresh scan (D-067), and `clean`, `plan -o`, `apply`, `history` and `restore` with their receipts and exit codes (M3b). Then profiles and their check (D-068), the TUI's review, progress and result screens (D-069), and a prompt that Ctrl+C interrupts, the typed size and `--dry-run` (D-070) (M3c). Deferred past M3: a profile run noticing a grant whose rule was removed from the config, and changed retention advice, which profiles don't hold.

Build:
- spike S1, then the platform executor and runner
- the journal (intent before effect, reconciliation of indeterminate items)
- plans (frozen targets, revalidation that only subtracts) and receipts
- methods `delete` and `trash`; spike S4
- `clean` (interactive, `--dry-run`, bounded `--yes`), `plan -o`, `apply`, `history` and `restore`
- profiles (`profile create/review/list/remove`, `clean --profile`) with effective-scope freezing
- overlap and alias handling (04 §3.10), receipt privacy (04 §8a)

Verify:
- Every required failure scenario in 04 §10 has a test.
- Profile regressions: switching Careful to Balanced, switching Balanced to Thorough, a per-rule level or recent-window change, re-enabling a rule, same rule ID with a wider pattern, a replaced root, a changed method or retention, a removed grant, a new rule, and a once-Safe item becoming Review or unknown. Each one suspends or skips, and none of them widens.
- Overlap regressions: an alias across a protected root, a hard link with an unselected sibling, a parent/child overlap, a method conflict.
- The native-profile suite passes on all three CI OSes.
- A seeded confinement property runs 10k cases in CI (D-061: each failure names the seed that reproduces it, with no dependency).
- End to end on `demo-home`: `clean --only builtin.cargo.target --yes` removes exactly the snapshotted set and writes a receipt. `restore` brings back trash-backed items on Linux and macOS (S4, D-062: kipple's own restore on both; Windows has no trash method in v0.1).

## M4: Agent integrations

Build:
- spike S5
- Codex inactive releases (with running-release protection) and logs
- Claude Code logs, worktree anomalies (report), registered worktree removal if its gate passes
- Pi and Cursor where verified
- npm cache as the first typed native operation (a semantic plan)

Verify:
- Fixtures reproduce the founder's observed layouts:
  - Codex standalone and app-server-daemon with 5+3 releases, one older release running: the active, running and rollback releases are kept
  - 8 agent worktrees in mixed states (Claude-made and other agents), each classified correctly with its agent label
- Negative fixtures (unknown layout, incomplete process inventory) block.
- The optional founder-machine smoke test follows the live-machine rules above.

## M5: Rule packs

Build:
- `pack add`, `remove`, `list`, `update`, `grant`, `revoke`, `new`, `check`, `try`
- lockfile, grants, update diffs
- a nightly fuzz job, with a target for manifest parsing (04 §10)

Verify:
- Ordinary tests install an example pack through an injected transport (no network): it installs with `--sha256`, shows `needs grant`, and after `grant` cleans only inside its root. A separate integration test, marked as such, uses a loopback HTTPS server with a test-only CA trusted only by that test client. Production rejects `http://` URLs.
- An update that widens its pattern asks for a new grant.
- `pack check` rejects every invalid case in 06 §3.
- Fixtures cover an item outside any repository (tracked count 0, eligible), one under an unreadable parent (unknown, blocked), and one where the fixture home sits inside a repository that tracks files in the matched item (blocked).

## M6: Site and docs

M6 shares no files with the crates, so its design direction and the site's skeleton may start alongside M3 (founder, 2026-10-16). The Safety pages and every command shown wait for the behaviour they describe, and 09 §3's rule that each command comes from a tape or test that ran in CI still holds.

Build ([09-site-and-docs.md](09-site-and-docs.md)):
- the Astro and Starlight site, with the landing page built through the design process and to 09 §3's direction (minimal, distinct, nothing generic) and page-weight budget
- Safety pages checked against tests
- tapes and the `/next/` preview deploy
- turn on `cargo xtask site` in CI

Verify:
- `cargo xtask site` passes (build, links, Lighthouse budget).
- The founder approves the design direction before the build and the result after.

## M7: Quality and hardening

M7 adds no features. It gets what M0 to M6 built ready to ship: shared logic moves into a library crate, each fact gets one owner, weak spots are hardened, tests that can't fail go, and speed and CI get budgets measured at M7. Nothing in M7 may weaken a check in [04-safety-model.md](04-safety-model.md).

Nothing is released yet, so a format M7 changes is changed in place: its schema, baseline and samples are regenerated in the commit that changes it, with the reason in the commit message, and the one reader reads only the current shape. No version bump, second reader or migration. A file from an earlier build is refused with a message, never half-read.

Build:
- the library split: a fifth crate, `kipple-app`, between the library crates and the frontends. It holds what both frontends need: config loading, validation and editing; `Machine` and the built-in setup; the keep, level and project-root actions with the rescan they start; the project-root search; the level totals; the scan notices; `human` and the JSON model. The binary keeps clap, the TUI, themes, prompts and rendering, so it has only its views and keys, and no setting or total is worked out in two places
  - why before a GUI: the CLI and the TUI are already two frontends over this logic, and it is already written twice (`Policy` is built in `config.rs` and again in `tui/onboarding.rs`), so reuse is demonstrated before any GUI (D-029 stands). 0.1.0 publishes the crates, so the boundary moves before there is a published API to break
  - `crates/kipple/tests/compat.rs` reaches the built-ins and the pack reader through `kipple-app`, because D-031 rejects dev edges too
  - a new decision entry amends D-002 (five crates), D-017 (all five published; the name was free on crates.io on 2026-11-06) and D-031 (`kipple-app` → all three, the binary → core, platform and `kipple-app`)
  - `check-arch`, `AGENTS.md` 4 and 17, [05-architecture.md](05-architecture.md) (the crate list, the edges, the composition root), [09-site-and-docs.md](09-site-and-docs.md) and [10-ci-and-release.md](10-ci-and-release.md) (lockstep versions; publish order `kipple-core`, `kipple-adapters` and `kipple-platform`, `kipple-app`, `kipple`) change with it
- one source of truth:
  - the core owns on-disk size, reclaimable size and one saturating total, and every frontend sum uses them
  - one function gives the scan notices, and one folds paths for comparison
  - the wording left in `doctor`, `explain` and `scan` moves to `human`
  - free-string JSON values (omission kinds, activity reasons, `seen`, hold reasons, doctor's config source) become closed enums in the schemas, each value spelled as it is today. Doctor's root `source`, which is formatted text, is replaced by a closed kind plus free detail. The baselines are refreshed in place, and 08 §8's sentence on free strings changes. From 0.1.0 a new value in a closed field breaks a written record, so it needs the format's next version and 0.2
  - `check-arch` reads match arms with `syn` and flags any frontend that maps a core enum to text
  - the gate compares the dependency tables in [decisions.md](decisions.md) and [10-ci-and-release.md](10-ci-and-release.md) with `Cargo.lock` and the workflow pins, and `cargo xtask fix` rewrites their version columns, so a dependabot PR needs one command. The publish date, which no lock file holds, stays a hand edit
- hardening:
  - every size sum saturates
  - `/proc` files are read as bytes, so one name that isn't UTF-8 doesn't protect every finding
  - reads of `/proc/<pid>` files are bounded, well above an ordinary browser's or JVM's `maps`. Past the bound a field counts as unread, as a failed read does today, so the process stays uninspected and never reads as idle
  - a stored plan gets a total limit, 1 GiB as a plan file has. A plan over it is refused before the run is stored, as D-065 refuses a header or tree too large, and the reader holds the same limit, so no run is stored that couldn't be read back. D-065 changes with it
  - the Windows walker counts a mount point as skipped, as Linux does
  - typed errors replace `Result<_, String>` on public functions, and items only tests use leave the public surface
  - a retention rule, so a mutating command stops reading nearly every run ever kept. Past a set age a run is dropped, under the run lock, only when every item has ended, no item can still be restored from the trash, and none is stranded or indeterminate. A run that is unreadable, or that can't be judged, stays. 04 §8a and D-065, which keep runs until the user removes them, change with it
  - frozen profile roots survive a reboot that renumbers devices: a root is held by a stable file system ID from the platform (UUID or equivalent) with its inode or file ID, never by the inode alone. A root with no such ID stays suspended. `kipple.profile/v1` changes in place and its baseline is refreshed; a profile made before is refused with a message to recreate it. A run's anchors keep `FileIdentity`. 04 §3.4, §8a and §11 (a disk clone keeps the ID) and D-068 change with it
  - a profile run notices a grant whose rule left the config, and changed retention advice, and each suspends or skips. The advice is a new field in the same in-place change, so D-068 and 03 J6, which say a profile has no retention field, change too
  - golden history lines can only be appended: the gate compares each `HISTORY` table with the first parent of `HEAD`, a missing parent fails it, and the `check` job checks out with `fetch-depth: 2`
  - clippy's `undocumented_unsafe_blocks` is on, and `crates/kipple-platform/UNSAFE.md` lists each `unsafe` block with its invariant and its test, or a stated reason
- tests:
  - delete four tests that can't fail for a realistic bug: the compile-time `Send` checks `a_prepared_clean_can_go_to_a_worker` and `a_machine_can_go_to_a_worker`, and the colour copies `help_at_160_by_48_in_colour` and `settings_at_160_by_48_in_colour`. `the_bucket_home_at_160_by_48_in_colour` stays as the smoke test
  - shorten the `bench_tree` tests, the test that waits out a killed lock holder, and `an_answer_is_whole_up_to_64_mib_and_cut_past_it`, whose bound becomes injectable
  - add generated-sequence tests for `App::update`, for the journal and reconcile across restarts, for config edits and for deduplication
  - add fuzz targets for path normalization and the stored-format decoders, run by M5's nightly job
  - run the TUI's Review path and its snapshots on Windows
  - add a native test of a project on an unmounted file system, at the scan and between scan and apply
  - name each scenario and property of 04 §10 with its test, in 04 §10
- performance:
  - ask git once per repository per scan instead of twice per finding, filtering the listing in memory and keeping the size limit, the deadline and the identity checks. Both limits now cover a whole repository, so a repository past either falls back to per-finding questions instead of holding every finding in it. Apply still asks per item. D-066 changes with it
  - share process paths in the activity index instead of storing every prefix per process
  - cache the TUI's lines and per-node sums behind a revision that `update` bumps
  - cap the walker's pool at 8 threads
  - add a `[profile.release]` with LTO, one codegen unit and strip, keeping unwinding for the workers' `catch_unwind`
  - add `cargo xtask bench-scan`, which times the scan budgets below, prints the process count and fails below 1,000 processes
  - `bench-tui` gains a 20,000-finding tree
- CI and tooling:
  - a build cache saved from `main` only, and never in the release workflow
  - `push` runs limited to `main`, so a pull request runs once
  - `timeout-minutes` on every job
  - format and typos checked before anything compiles
  - `verify-demo` stops repeating what the gate already ran, keeping the `test-roots` tests on all three OSes
  - per-test times uploaded from every run, a 30 s limit on each default test in `.config/nextest.toml`, and a binary size step that fails past the budget
  - grouped dependabot updates
  - `check-style` reads macro bodies, and `check-docs` reads the site tapes

Verify:
- `crates/kipple/src` holds no config or policy logic: `cargo xtask check-arch` fails on a fixture that builds a `Policy` or edits config in the binary, one that reaches `kipple-adapters` from it, and one that adds clap or ratatui to `kipple-app`.
- `cargo xtask check-arch` fails on a fixture for each wording form: a constant, `.into()`, `concat!`, an `if` value, a nested match and a multi-line arm.
- `cargo xtask check-compat` fails when a closed enum's value is renamed: the refreshed baselines list the enums, and a fixture in `xtask/src/compat.rs` renames one.
- The gate fails when either dependency table differs from `Cargo.lock` or the workflow pins, and `cargo xtask check-docs` fails on a tape with a removed flag.
- `UNSAFE.md` lists every `unsafe` block, and the gate fails a block with no `SAFETY` comment.
- None of the four deleted tests remains, and every scenario and property of 04 §10 names its test, a native-profile test where 04 §10 asks for the real platform.
- Each generated-sequence test runs at least 500 sequences in the gate, and a run of each fuzz target finds no crash in 10 minutes. The `App::update` sequences keep the cursor on a line and the selection to shown findings, and start no apply without a review.
- The TUI's Review path and its `clean__*` snapshots pass on Windows.
- Hardening, each by a test:
  - a total over three 8 EiB sizes saturates in the core, the walker and the TUI
  - a non-UTF-8 `mountinfo` fixture gives a complete inventory, a malformed line still leaves it partial, and a process whose name isn't UTF-8 is inspected
  - a `/proc` field past its bound reads as unread and the finding it could name stays held
  - a planted oversized plan is refused, and a plan over the limit is refused before its run is stored
  - a native Windows test: a volume mounted inside a finding counts as skipped and holds the finding
  - over generated sequences with restarts and failed store calls, retention never drops a run that is unfinished, unreadable, stranded, indeterminate or still restorable from the trash, and `restore` still finds every item it could restore before
  - a different file system at the same path with the same inode suspends, a root with no stable ID stays suspended, the same file system under a new device number holds, and a profile from an earlier build is refused with the message
  - a removed grant's rule and changed retention advice each suspend or skip, and neither widens
  - the gate fails on a fixture that edits a golden history line and passes on one that appends
- Faster answers stay the same answers:
  - tracked counts and holds equal the per-finding query's over generated repositories with case-folding, non-ASCII, link and nested-repository fixtures, and a repository past the limits answers per finding
  - activity results are unchanged on generated process tables
  - paths differing only in case still match in policy, config and activity through the one fold
  - drawn output is identical with and without the TUI's cache across a generated sequence
- Scan and TUI budgets hold on the founder's workstation (32 threads, NVMe), on generated trees:
  - `cargo xtask bench-scan`: a warm scan of the 1M reference tree under home, with at least 1,000 processes running, takes under 1 s and peaks under 64 MB
  - `cargo xtask bench-scan`: a scan with 2,000 findings in one repository takes under 1 s
  - `cargo xtask bench-tui`: keypress-to-frame p95 is under 50 ms at 20,000 findings
- The `x86_64-unknown-linux-musl` binary, built with `[profile.release]`, is under 6,000,000 bytes, and the CI step fails past that.
- The default `cargo nextest run` takes under 5 s of wall time on the founder's workstation once built, the generated-sequence tests included, and a default test past 30 s fails.
- CI: with a warm cache the Windows `check` job takes under 270 s, a pull request's run has a p90 wall time under 300 s over 20 runs, each commit runs once, and every job has `timeout-minutes`.

After M7, CI holds the binary size and the per-test limit, and nothing else here. The scan, TUI, test-run and CI times are figures measured at M7: a later slowdown shows only when someone runs the tools again.

## M8: Release pipeline and 0.1.0

Build ([10-ci-and-release.md](10-ci-and-release.md)):
- release-plz and dist configured as specified
- a GitHub App token, Homebrew tap and AUR jobs, the distro smoke matrix
- crates.io publishing of the five crates and the npm package `@ayagmar/kipple`, both from the tag after the pre-publication gate (10 §3, D-017)
- the 0.1.0 baseline refresh (10 §3, 08 §8), the point from which compatibility is held: from there a break bumps the format's version and a reader keeps every older one

Verify:
- A rehearsal on a fork or branch produces every artifact, checksum, installer, attestation and SBOM without publishing, and `cargo publish --dry-run` and `npm pack` succeed for every package.
- The installers run on clean containers and VMs.
- The release-candidate site is promoted.
- `release-readiness-certification` reaches READY.
- Then tag `v0.1.0`.

## After 0.1

Features ship as 0.1.x releases as each one is ready (D-017); a change that breaks the config, JSON schemas or CLI makes 0.2. Each item goes through the same milestone loop: spec, fixtures for every upstream layout, review, and a green gate on three OSes. Rough order:

| Release | Theme |
| --- | --- |
| 0.1.x | Scheduled runs from a profile (systemd user timer, launchd agent, Task Scheduler entry, created and removed by `kipple schedule`). More agents (Gemini CLI, OpenCode, Copilot CLI, Aider). More ecosystems: Go, Bun, Flatpak, Homebrew, Xcode DerivedData, Docker and Podman native prune, emptying the Trash, a Downloads inventory (old installers as Review, never Safe). A curated pack index |
| 0.1.x | Report-only listing of leftover agent processes (orphaned MCP servers, headless browsers) with their memory use; kipple never kills them. Pack signatures and revocation. Store-specific session adapters (Pi first, since it is file-per-session with a documented in-app delete; Codex next, since its rollouts have no upstream retention), each needing verified semantics, fixtures and activity evidence. Model stores (Ollama manifests and blobs) |
| 0.1.x | `analyze`: a full-disk explorer that knows what it is looking at |
| 0.1.x or later | Privileged native operations, only after a separate reviewed design: a narrow privileged helper (never an elevated TUI or plugin host), typed operations, verified executable and config identity, re-checks after elevation, receipts. Covers pacman, apt, dnf and journald. It is the riskiest item here and ships only when its design review reaches READY |
| GUI | Deferred; planned after the CLI and TUI settle. The core stays GUI-ready (D-002, D-029), and M7 moves what a second frontend needs into `kipple-app`. Later, a Tauri or native app would be a second frontend over `kipple-app` and the same core |
| Maybe | An MCP server so agents can ask kipple what they can safely clean, via the same engine and grants |
