# Decisions

Each entry gives the decision, the reason and the date. Dependencies are recorded here when they are added to `Cargo.toml`. The candidate table in §2 was verified on 2026-10-08 and must be rechecked at M0.

## 1. Product and architecture decisions

| ID | Date | Decision | Why |
| --- | --- | --- | --- |
| D-001 | 2026-10-08 | Name: **kipple**. Runner-up: shedkit. | Strong story hook and precise meaning. Free on crates.io, Homebrew, AUR and Repology. Accepted risk: npm `kipple` 0.0.2 (8 downloads a month, abandoned since 2023) installs a `kipple` binary. Copy says "popularized by Philip K. Dick", not "coined". The founder makes the final call. |
| D-002 | 2026-10-08 | Four crates: `kipple-core`, `kipple-platform`, `kipple-adapters`, `kipple` (bin). | Clear authority boundaries without abstraction theater. A GUI becomes a second composition root. |
| D-003 | 2026-10-08 | Integrations and rules propose. The core decides and executes. Frontends review and authorize. | A single mutation boundary, the same policy for every frontend, and no plugin can hold deletion authority. |
| D-004 | 2026-10-08 | Category, eligibility, method and selection are separate concepts. | A manifest's category must never authorize deletion. |
| D-005 | 2026-10-08 | Third-party extension is declarative TOML rule packs only. No subprocess plugins, and WASM only for a demonstrated need. | Remote install doesn't need remote code. This follows mise's move away from executable plugins. |
| D-006 | 2026-10-08 | Typed built-in integrations plus a declarative policy layer. | Release grouping, git worktree state and running-binary identity can't be expressed safely as globs. |
| D-007 | 2026-10-08 | Default method: permanent delete for verified rebuildable items the user selected; OS trash opt-in; no custom quarantine in v0.1. | Users want space back, and trash doesn't free it. Accounting stays honest about both. |
| D-008 | 2026-10-08 | Sessions, checkpoints and models are report-only in v0.1, with native retention advice. | They are linked stores, and age is no proof something is unused. The founder mines agent sessions. |
| D-009 | 2026-10-08 | No root or elevation in v0.1. System stores get advice only. | The pacman cache is root-owned. Privilege orchestration is its own project. |
| D-010 | 2026-10-08 | Confinement must be proven per OS before any destructive release. The spike starts with `cap-std`. | A path check followed by a recursive delete is unsafe under concurrent change. |
| D-011 | 2026-10-08 | `--yes` needs a bounded, frozen scope. New rules never join automation silently. | Suppressing a prompt is not a grant of authority. |
| D-012 | 2026-10-08 | `--json` is one envelope. `--events jsonl` is a stream with sequence numbers. | Simple consumers shouldn't need a streaming parser. |
| D-013 | 2026-10-08 | Core is synchronous over bounded worker threads. No async runtime in the library crates. | Filesystem work blocks anyway. This keeps the API simple for a GUI. |
| D-014 | 2026-10-08 | Directory walking uses `ignore`'s parallel walker (provisional, with every ignore/hidden filter disabled; see 05 §3). `jwalk` is rejected. Spike S3 confirms it against `dua-core` and plain `read_dir`. | jwalk is archived and unmaintained. dua-core is promising but only weeks old. |
| D-015 | 2026-10-08 | kipple's own dirs come from `etcetera`'s app strategy (XDG on Linux and macOS, Known Folders on Windows), for every frontend including a GUI. | This matches what CLI users expect on macOS, and CLI and GUI must share config, grants and receipts. |
| D-016 | 2026-10-08 | `cargo xtask` is the task runner. No just or make. | `just` and `make` recipes run in a shell and break on Windows without `sh`. An xtask runs the same on all three OSes. See 10-ci-and-release.md §1. |
| D-017 | 2026-10-08 | Release with release-plz (lockstep versions, changelog, a single `vX.Y.Z` tag on `kipple`) plus dist (build, installers, checksums, attestations, SBOM). No crates.io publishing in v0.1. No self-updater. | Both are maintained, and they cover the pipeline without custom scripts. Publishing only from the tag keeps every upload behind the gate. |
| D-018 | 2026-10-08 | One document format, the pack, for built-in and third-party rules. A normative schema (06 §3). Fixtures built in test code, not in a new file format. | One contract to validate and document. No accidental DSLs. |
| D-019 | 2026-10-08 | TUI on ratatui. FrankenTUI (`ftui` 0.9.0) was considered and rejected. | Its license (`LicenseRef-MIT-OpenAI-Anthropic-Rider`) voids rights for anyone acting for OpenAI or Anthropic, counts analysis and evaluation as "use", and must travel with every distribution. A combined kipple distribution would have to carry that rider, so it couldn't be offered as plain MIT, and distro packaging would be at risk ([upstream LICENSE](https://github.com/Dicklesworthstone/frankentui/blob/main/LICENSE)). It also needs nightly (`nightly-2026-08-31`), calls itself WIP, and is 20 crates. Ideas adopted: inline mode, a single terminal writer, terminal restore on panic. |
| D-020 | 2026-10-09 | TUI home groups findings into Safe / Review / Protected buckets, derived by ordered, exhaustive host policy from composable consequence facts (04 §2a), with Agents/Dev/System as secondary grouping. Validated against a lens-first layout in M2. | Normal users get a plain answer, developers keep the evidence. From the deep-research report, modified after the Codex challenge so that Safe is never a rule's claim. |
| D-021 | 2026-10-09 | Unattended `clean --yes` beyond `--only` goes through profiles that freeze the effective scope (digests, roots, retention, methods), not just rule IDs. Any change suspends the entry until reviewed. | Same-ID rule widening must not silently grow cron authority. |
| D-022 | 2026-10-09 | `kipple doctor` (read-only) from M1, reusing the probe diagnostics. | Empty or partial scans need an explanation, not guesswork. |
| D-023 | 2026-10-09 | Roots resolve through each tool's upstream precedence first (CODEX_HOME, CLAUDE_CONFIG_DIR, Pi session dir), and Linux support is capability-based, never keyed on distro names. | Correct discovery on relocated setups, and on derivatives without a distro table. |
| D-024 | 2026-10-09 | Worktrees use a generic Git integration with agent labels, not a Claude-specific one. | The verified public report shows most abandoned worktrees came from other agents, and Claude already sweeps its own. |
| D-025 | 2026-10-09 | Session cleanup stays out of v0.1. Pi is the first candidate in 0.3, then Codex. Privileged operations wait for a separate design (0.5). | The founder mines sessions. Sessions are linked stores with upstream exceptions. A sudo prompt isn't a privilege boundary. |

## 2. Candidate dependencies (verified 2026-10-08, not yet added)

Rule: newest stable release that is at least 7 days old. Recheck at M0.

| Crate | Version | Published | Use | Note |
| --- | --- | --- | --- | --- |
| Rust toolchain | 1.99.0 | 2026-10-01 | pinned in `rust-toolchain.toml` | edition 2024 |
| clap / clap_complete | 4.6.7 / 4.6.11 | 2026-09-14/15 | CLI, completions | |
| ratatui (+ ratatui-crossterm) | 0.30.2 | 2026-06-19 | TUI | |
| crossterm | 0.29.0 | 2025-04-05 | terminal backend | slow release cadence, active repo |
| serde / serde_json | 1.0.229 / 1.0.151 | 2026-07 | serialization | |
| toml | 1.1.6 | 2026-09-10 | config, rules, lockfile | 1.1.7 too new |
| schemars | 1.2.2 | 2026-07-27 | JSON Schemas for `--json` and rules | |
| thiserror | 2.0.21 | 2026-09-23 | errors | |
| cap-std / cap-fs-ext | 4.0.3 | 2026-08-20 | confined executor (spike S1) | **>= 4.0.3 required**: [GHSA-hp8f-xmx4-4qrg](https://github.com/sunfishcode/cap-std/security/advisories/GHSA-hp8f-xmx4-4qrg) (published 2026-08-20), `manually::open` follows symlinks through a trailing slash. Also GHSA-hxf5-99xg-86hw (Windows device names, fixed in 3.4.1) |
| rustix | 1.1.5 | 2026-09-16 | Unix syscalls not covered by cap-std | |
| windows-sys | 0.61.2 | 2025-10-06 | Windows file IDs, process identity | |
| trash | 5.2.9 | 2026-09-13 | OS trash | list/restore only on Windows and freedesktop, **not macOS** (see Q4) |
| ignore | 0.4.33 | 2026-08-04 | parallel walking | |
| rayon | 1.12.0 | 2026-04-14 | sizing pool | maybe unnecessary, decided in S3 |
| crossbeam-channel | 0.5.17 | 2026-09-05 | bounded event channel | |
| sysinfo | 0.39.6 | 2026-07-09 | process snapshot | MSRV 1.95; compare with a direct `/proc` read in S2 |
| etcetera | 0.11.0 | 2025-10-28 | platform dirs | |
| semver | 1.0.28 | 2026-04-04 | release ordering | |
| jiff | 0.2.37 | 2026-09-12 | timestamps | |
| ulid | 3.0.0 | 2026-07-16 | receipt and plan IDs (sortable) | |
| sha2 | 0.11.0 | 2026-03-25 | pack digests (`--sha256`) | |
| ureq | 3.4.2 | 2026-09-13 | pack download (bin only) | |
| bytesize | 2.7.0 | 2026-08-02 | human sizes | humansize is stale |
| nucleo-matcher | 0.3.1 | 2024-02-20 | TUI fuzzy filter | slow cadence, used by Helix |
| tracing / tracing-subscriber | 0.1.44 / 0.3.23 | | diagnostics to stderr and log file | |
| insta / proptest / tempfile / assert_cmd / assert_fs | 1.48.0 / 1.11.0 / 3.27.0 / 2.2.2 / 1.1.4 | | tests | |
| cargo-nextest / cargo-deny / cargo-machete / typos-cli | 0.9.146 / 0.20.2 / 0.9.2 / 1.50.3 | | gate tools | |

Site and release tool versions are recorded in [09-site-and-docs.md](09-site-and-docs.md) and [10-ci-and-release.md](10-ci-and-release.md).
