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

## Spikes (timeboxed, inside the milestone that needs them)

| ID | When | Question | Output |
| --- | --- | --- | --- |
| S1 | M3 | Can `cap-std` (>= 4.0.3) plus std give confined, handle-relative removal on Linux, macOS and Windows: root identity, no link or junction escape, defined behaviour for new children? | Prototype plus `native`-profile tests on all three CI runners. The executor design and its findings go in decisions.md |
| S2 | M1 | Running-executable identity: `sysinfo` vs direct `/proc`, `proc_pidpath` and `QueryFullProcessImageNameW`. Speed, identity (inode or file ID), failure modes. | A decision in decisions.md |
| S3 | M1 | Walker and sizing: configured `ignore` vs `dua-core` vs `read_dir` with `rayon`. | Numbers from the procedure below, and a decision |
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

Build:
- state machine plus a pure renderer: Safe / Review / Protected buckets with Agents / Dev / System grouping, detail pane (what it is, why it's here, why kipple thinks it's safe, what you lose, how it comes back), streaming sizes, fuzzy filter, selection and the `c` Safe shortcut, help, narrow layout, `NO_COLOR` and `--ascii`
- a lens-first variant behind a dev flag, used only for the M2 comparison and then deleted
- vhs tapes for first scan and navigation

Verify:
- State-transition tests pass.
- Buffer snapshots at 80×24 and 160×48 match.
- Keypress-to-frame p95 is under 50 ms while scanning the 1M reference tree, measured by `xtask bench-tui`.
- The founder compares the bucket and lens layouts on the same fixture data, picks one, and signs off on the feel. The loser is removed.

## M3: Safe mutation (two sessions)

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
- Profile regressions: same rule ID with a wider pattern, a replaced root, a changed method or retention, a removed grant, a new rule, and a once-Safe item becoming Review or unknown. Each one suspends or skips, and none of them widens.
- Overlap regressions: an alias across a protected root, a hard link with an unselected sibling, a parent/child overlap, a method conflict.
- The native-profile suite passes on all three CI OSes.
- `proptest` confinement properties run for 10k cases in CI.
- End to end on `demo-home`: `clean --only builtin.cargo.target --yes` removes exactly the snapshotted set and writes a receipt. `restore` brings back trash-backed items on Linux and Windows, and on macOS as S4 decided.

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
- a nightly fuzz job

Verify:
- Ordinary tests install an example pack through an injected transport (no network): it installs with `--sha256`, shows `needs grant`, and after `grant` cleans only inside its root. A separate integration test, marked as such, uses a loopback HTTPS server with a test-only CA trusted only by that test client. Production rejects `http://` URLs.
- An update that widens its pattern asks for a new grant.
- `pack check` rejects every invalid case in 06 §3.
- Fixtures cover an item outside any repository (tracked count 0, eligible), one under an unreadable parent (unknown, blocked), and one where the fixture home sits inside a repository that tracks files in the matched item (blocked).

## M6: Site and docs

Build ([09-site-and-docs.md](09-site-and-docs.md)):
- the Astro and Starlight site, with the landing page built through the design process
- Safety pages checked against tests
- tapes and the `/next/` preview deploy
- turn on `cargo xtask site` in CI

Verify:
- `cargo xtask site` passes (build, links, Lighthouse budget).
- The founder approves the design direction before the build and the result after.

## M7: Release pipeline and 0.1.0

Build ([10-ci-and-release.md](10-ci-and-release.md)):
- release-plz and dist configured as specified
- a GitHub App token, Homebrew tap and AUR jobs, the distro smoke matrix

Verify:
- A rehearsal on a fork or branch produces every artifact, checksum, installer, attestation and SBOM without publishing.
- The installers run on clean containers and VMs.
- The release-candidate site is promoted.
- `release-readiness-certification` reaches READY.
- Then tag `v0.1.0`.

## After 0.1

| Version | Theme |
| --- | --- |
| 0.2 | Curated pack index, more agents (Gemini CLI, OpenCode, Copilot CLI, Aider), Docker and Podman native prune, emptying the Trash, Flatpak, Homebrew, Xcode DerivedData, Go, Bun, a Downloads inventory (old installers as Review, never Safe) |
| 0.3 | Pack signatures and revocation, store-specific session adapters (Pi first, since it is file-per-session with a documented in-app delete; Codex next, since its rollouts have no upstream retention), each needing verified semantics, fixtures and activity evidence. Model stores (Ollama manifests and blobs). crates.io publishing |
| 0.4 | `analyze`: a full-disk explorer that knows what it is looking at |
| 0.5 | Privileged native operations, only after a separate reviewed design: a narrow privileged helper (never an elevated TUI or plugin host), typed operations, verified executable and config identity, re-checks after elevation, receipts. Covers pacman, apt, dnf and journald |
| GUI | A Tauri (or native) app as a second composition root over the same core |
| Maybe | An MCP server so agents can ask kipple what they can safely clean, via the same engine and grants |
