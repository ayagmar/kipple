# Decisions

Each entry gives the decision, the reason and the date. Dependencies are recorded in §2 when they are added to `Cargo.toml`. The candidates in §3 were verified on 2026-10-08 and are rechecked when each one is added.

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
| D-014 | 2026-10-08 | Directory walking uses `ignore`'s parallel walker (provisional, with every ignore/hidden filter disabled; see 05 §3). `jwalk` is rejected. Spike S3 confirms it against `dua-core` and plain `read_dir`. | jwalk is archived and unmaintained. dua-core is promising but only weeks old. **Superseded by D-036 (2026-10-10).** |
| D-015 | 2026-10-08 | kipple's own dirs come from `etcetera`'s app strategy (XDG on Linux and macOS, Known Folders on Windows), for every frontend including a GUI. **The layout stands; the library was dropped by D-037 (2026-10-10).** | This matches what CLI users expect on macOS, and CLI and GUI must share config, grants and receipts. |
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
| D-026 | 2026-10-09 | Carefulness levels (Careful, Balanced default, Thorough) move only the Safe/Review line (the method is a separate setting), set during first-run onboarding with live numbers for the machine, and can be overridden per rule. | Too careful and kipple cleans nothing; too loose and it breaks things. Protected and the invariants never move. |
| D-027 | 2026-10-09 | Every deletion needs a review screen and explicit confirmation, except the automation forms the user set up. | Users expect to confirm before anything is deleted. |
| D-028 | 2026-10-09 | Every rule carries `why`, `loss` and optional upstream `references`, can be tuned or disabled in config, and gets a generated docs page. The `p`/`P` keys keep an item or a rule permanently (`k` stays vim navigation). | Every action has a reason, is configurable, and is documented. |
| D-029 | 2026-10-09 | No GUI before the CLI and TUI reach 1.0. The core stays GUI-ready. | Focus. A GUI doubles the UX surface before the engine has proven itself. |
| D-030 | 2026-10-10 | `unsafe_code` is `deny` in `[workspace.lints]`, and every crate root except `kipple-platform` adds `#![forbid(unsafe_code)]`. | Cargo can't override a single workspace lint for one crate, so the strictest level that still lets the platform crate use reviewed `#[expect(unsafe_code)]` goes in the workspace, and the other crates forbid it themselves. |
| D-031 | 2026-10-10 | `check-arch` keeps an allowlist of kipple-to-kipple edges (adapters → core, platform → core, the binary → all three) and rejects every other edge of any kind (normal, dev, build). `xtask` may depend on any crate. `target_os` anywhere in a `.rs` file under `crates/` outside `crates/kipple-platform` fails. `xtask` is not scanned: it is a dev tool, and later tasks such as `test-native` may need OS-specific code. | An allowlist rejects new edges too (05 §1), and a dev-dependency crosses a boundary just as much as a normal one. |
| D-032 | 2026-10-10 | The `kipple` package has a library target that exposes the clap definition. `gen-docs` renders `--help` and `--version` through clap's own handling of those flags, and writes `site/src/generated/cli.md` until the site exists. | The reference is exactly what the binary prints, without running it and without a hidden subcommand (09 §6). |
| D-033 | 2026-10-10 | `xtask` launches cargo and the gate tools through one `#[expect(clippy::disallowed_methods)]` in `xtask/src/cmd.rs`. It runs `cargo-machete` directly, not as `cargo machete`. | xtask is the dev-only task runner, so it is the one place outside the platform runner that spawns processes. Launched as `cargo machete` from inside `cargo run`, cargo-machete 0.9.2 took `machete` for a directory to scan and failed. |
| D-034 | 2026-10-10 | The project license is not chosen yet. The crates are `publish = false` with no `license` field, and `deny.toml` ignores private crates for license checks. `Unlicense` (from `memchr`'s `Unlicense OR MIT`) is on the allowlist. | Choosing a license is the founder's call. Until then nothing is published. |
| D-035 | 2026-10-10 | Running-executable identity (spike S2) comes from direct OS APIs in `kipple-platform`, not `sysinfo`. Linux: `readlink` and `stat` on `/proc/<pid>/exe` (std only), giving device, inode and link count of the file actually running. macOS: `proc_listallpids` and `proc_pidpath` for the path; the identity (device, inode, link count) is the vnode behind the process's first file-backed region, via `proc_pidinfo(PROC_PIDREGIONPATHINFO)` (`libc`), and it counts only when that region's path is the path `proc_pidpath` returns. Windows: `K32EnumProcesses`, `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)`, `QueryFullProcessImageNameW`, then volume serial and file index of the image file (`windows-sys`). Every PID gets an explicit outcome: identified, path only, denied, exited, gone, no executable, or other error. Only exited and gone mean "not running", and gone needs positive evidence (`ESRCH`, or the PID's `/proc` entry missing). Path only, denied and other error are unknown. A `/proc` mounted with `hidepid` or `subset` makes the whole inventory partial. | `sysinfo` 0.39.6 returns `exe() == None` for a permission error, a zombie and a kernel thread alike (159 of 166 processes on the Linux runner were denied), strips the kernel's ` (deleted)` marker so a binary replaced in place reports the new file at that path, gives no file identity, and lists threads as processes unless told not to. `stat` on a reported path gives the new file's identity after an in-place replacement on Linux and macOS. The direct probes identified that case on all three OSes, stayed unknown where they could not confirm an identity, and took under 17 ms per snapshot. Evidence in §4.1. |
| D-036 | 2026-10-10 | Directory walking and sizing (spike S3) use kipple's own walker in `kipple-platform`: std `read_dir`, metadata from each `DirEntry` (no-follow), and one task per directory on a bounded `rayon` pool. The walker itself never reads ignore files or git config, never skips hidden entries, never follows links, stays on the root's file system, honours the selector's `max_depth`, and prunes artifacts and exclusions before descending. `ignore` and `dua-core` are not used. Supersedes D-014. **Accepted exception:** on macOS at 1M entries it is 17% behind `dua-core`, outside the S3 acceptance rule; the founder kept the walker on 2026-10-10. If macOS misses a scan budget, the platform walker can read bulk metadata (`getattrlistbulk`) itself. | Warm, it was fastest on Linux (1M: 1226 ms; `ignore` 1800, `dua-core` 2220) and Windows (1M: 2334 ms; 3960 and 3853), and cold on Linux (1M: 9949 ms; 10540 and 10643). On Linux and Windows `ignore` came within 10% only in cold runs, and was 44–77% slower warm. It `lstat`s every entry and `stat`s every directory by full path, while std on Linux (glibc) and macOS stats relative to the open directory. On macOS the walker was fastest at 100k and 17% behind `dua-core`'s bulk metadata at 1M. Owning the walker also leaves room for the handle-relative probes of spike S1. Evidence in §4.2. |

| D-037 | 2026-10-10 | kipple's own directories come from the platform's root resolver, not `etcetera`, keeping the D-015 layout: `kipple` under `$XDG_CONFIG_HOME`, `$XDG_DATA_HOME` and `$XDG_CACHE_HOME` on Linux and macOS (defaults under `$HOME`), and `kipple\config`, `kipple\data` under the `RoamingAppData` Known Folder and `kipple\cache` under `LocalAppData` on Windows, read through `SHGetKnownFolderPath`. A relative `XDG_*` value leaves the directory unknown. | `etcetera` 0.11.0 reads the process environment itself, which bypasses the one snapshot the composition root takes (05 §1) and the injected roots tests depend on (08 §5). It also silently ignores a relative `XDG_*` value and uses the default, where 05 §1 requires unknown, and it can't say where a directory came from, which `doctor` reports. The platform resolves the same roots, with their sources, for discovery anyway. |
| D-038 | 2026-10-10 | The platform runner is the one `Command::new` in kipple's crates. It starts a native command with no stdin and an empty environment plus `LC_ALL=C` (and `SystemRoot` on Windows), keeps at most 64 KiB of each output stream, and **kills the child it started** if the child misses its deadline. So far it runs only `--version` probes for `git`, `paccache` and `journalctl`, with a 2 s deadline, found on absolute `PATH` entries only. | A deadline that can't stop the child bounds nothing: a hung probe would hang `doctor`. "No process control" (04 §3.8) is about processes kipple didn't start, and now says so. A version probe needs no user environment, and a fixed one keeps its output parseable. |
| D-039 | 2026-10-10 | Native-backend tests live in test targets named `native` and run only in the `native` nextest profile (`.config/nextest.toml`; the default profile excludes them), through `cargo xtask test-native`, which refuses to run unless `KIPPLE_TEST_DISPOSABLE_HOST=1`. CI's `native` job runs them on all three OSes. | 08 §5: the process table and the Windows Known Folders are real OS state that a test can't redirect, so their tests must only run on disposable hosts. |

## 2. Dependencies and tools in use

Rule: newest stable release that is at least 7 days old. Verified 2026-10-10 (M0, and again for the M1 crates) against the crates.io API and the GitHub releases API.

| Dependency | Version | Published | Used by | Why |
| --- | --- | --- | --- | --- |
| Rust toolchain | 1.99.0 | 2026-10-01 | `rust-toolchain.toml` (with clippy, rustfmt) | edition 2024, newest stable |
| clap (derive) | 4.6.7 | 2026-09-14 | `kipple`, `xtask` | CLI parsing, and the source of the CLI reference |
| anyhow | 1.0.104 | 2026-07-18 | `xtask` only | error context in the dev tool (08 §4) |
| serde (derive) | 1.0.229 | 2026-07-18 | `xtask` | typed `cargo metadata` for `check-arch` |
| serde_json | 1.0.151 | 2026-07-20 | `xtask` | parsing `cargo metadata` |
| tempfile | 3.27.0 | 2026-03-11 | `xtask`, `kipple-platform` (dev) | disposable directories for the `bench-tree` and platform tests, removed on drop. Added 2026-10-10 (M1) |
| thiserror | 2.0.21 | 2026-09-23 | `kipple-core` | structured error enums that keep the upstream error (08 §4). Added 2026-10-10 (M1) |
| crossbeam-channel | 0.5.17 | 2026-09-05 | `kipple-core` | the bounded event stream (05 §2). A producer must wait on a full stream yet still notice a cancel, and std's `SyncSender::send_timeout` is unstable in Rust 1.99. Added 2026-10-10 (M1) |
| rayon | 1.12.0 | 2026-04-14 | `kipple-platform` | the walker's bounded thread pool (D-036). Its `either` dependency is held at 1.18.0 (2026-08-20) in `Cargo.lock`: 1.19.0 was published on 2026-10-06. Added 2026-10-10 (M1) |
| libc | 0.2.190 | 2026-10-02 | `kipple-platform`, macOS only | `proc_listallpids`, `proc_pidpath` and `proc_pidinfo` (D-035). The 1.0.0 releases are still alphas. Added 2026-10-10 (M1) |
| windows-sys | 0.61.2 | 2025-10-06 | `kipple-platform`, Windows only | process identity and file identity (D-035), Known Folders (D-037). Added 2026-10-10 (M1) |
| syn (transitive) | 3.0.6 | 2026-09-16 | via clap_derive, serde_derive | held at 3.0.6 in `Cargo.lock`: 3.0.7 was published on 2026-10-10 |

Every other crate in `Cargo.lock` is the newest stable release its dependents allow, and at least 7 days old on 2026-10-10.

| Tool or action | Version | Published | Where |
| --- | --- | --- | --- |
| cargo-nextest | 0.9.146 | 2026-09-21 | gate tests (0.9.148 is from 2026-10-08) |
| cargo-deny | 0.20.2 | 2026-07-09 | `deny` CI job, local gate |
| cargo-machete | 0.9.2 | 2026-04-15 | gate |
| typos-cli | 1.50.3 | 2026-09-25 | gate, pre-commit (1.51.x is from 2026-10-06) |
| `actions/checkout` | v7.0.1 (`3d3c42e5aac5ba805825da76410c181273ba90b1`) | 2026-07-20 | `ci.yml` |
| `taiki-e/install-action` | v2.87.23 (`861a07ce7084f55488e375df125cdc99bba60eb7`) | 2026-10-03 | `ci.yml` (Dependabot #1, merged 2026-10-10; v2.87.24 is from 2026-10-04 and was under 7 days old) |
| strace, GNU time, Python 3 | 6.8 (`6.8-0ubuntu2`), 1.9 (`1.9-0.2build1`, prints `UNKNOWN`), 3.12.3 | Ubuntu 24.04 release pocket; Python from runner image `ubuntu24/20261004.327` | `bench.yml` and `.github/scripts/bench.py`. **Exception to the newest-stable rule:** these come from the runner's Ubuntu archive and are not pinned, because they only measure, are never shipped and never gate. Each run logs the versions in its machine description |

Spike-only crates, used on the `spike/m1-s2-s3` branch for S2 and S3 and not a dependency of any crate on `main`. The spikes also used `libc`, `windows-sys` and `rayon` at the versions above. Verified 2026-10-10:

| Crate | Version | Published | Spike |
| --- | --- | --- | --- |
| sysinfo | 0.39.6 | 2026-07-09 | S2, compared against the direct probes |
| ignore | 0.4.33 | 2026-08-04 | S3 variant |
| dua-core | 4.1.0 | 2026-09-12 | S3 variant |

## 3. Candidate dependencies (verified 2026-10-08, not yet added)

Rule: newest stable release that is at least 7 days old. Recheck each one when it is added.

| Crate | Version | Published | Use | Note |
| --- | --- | --- | --- | --- |
| Rust toolchain | 1.99.0 | 2026-10-01 | pinned in `rust-toolchain.toml` | edition 2024 |
| clap / clap_complete | 4.6.7 / 4.6.11 | 2026-09-14/15 | CLI, completions | |
| ratatui (+ ratatui-crossterm) | 0.30.2 | 2026-06-19 | TUI | |
| crossterm | 0.29.0 | 2025-04-05 | terminal backend | slow release cadence, active repo |
| serde / serde_json | 1.0.229 / 1.0.151 | 2026-07 | serialization | |
| toml | 1.1.6 | 2026-09-10 | config, rules, lockfile | 1.1.7 too new |
| schemars | 1.2.2 | 2026-07-27 | JSON Schemas for `--json` and rules | |
| cap-std / cap-fs-ext | 4.0.3 | 2026-08-20 | confined executor (spike S1) | **>= 4.0.3 required**: [GHSA-hp8f-xmx4-4qrg](https://github.com/sunfishcode/cap-std/security/advisories/GHSA-hp8f-xmx4-4qrg) (published 2026-08-20), `manually::open` follows symlinks through a trailing slash. Also GHSA-hxf5-99xg-86hw (Windows device names, fixed in 3.4.1) |
| rustix | 1.1.5 | 2026-09-16 | Unix syscalls not covered by cap-std | |
| trash | 5.2.9 | 2026-09-13 | OS trash | list/restore only on Windows and freedesktop, **not macOS** (see Q4) |
| semver | 1.0.28 | 2026-04-04 | release ordering | |
| jiff | 0.2.37 | 2026-09-12 | timestamps | |
| ulid | 3.0.0 | 2026-07-16 | receipt and plan IDs (sortable) | |
| sha2 | 0.11.0 | 2026-03-25 | pack digests (`--sha256`) | |
| ureq | 3.4.2 | 2026-09-13 | pack download (bin only) | |
| bytesize | 2.7.0 | 2026-08-02 | human sizes | humansize is stale |
| nucleo-matcher | 0.3.1 | 2024-02-20 | TUI fuzzy filter | slow cadence, used by Helix |
| tracing / tracing-subscriber | 0.1.44 / 0.3.23 | | diagnostics to stderr and log file | |
| insta / proptest / assert_cmd / assert_fs | 1.48.0 / 1.11.0 / 2.2.2 / 1.1.4 | | tests | |
| cargo-nextest / cargo-deny / cargo-machete / typos-cli | 0.9.146 / 0.20.2 / 0.9.2 / 1.50.3 | | gate tools | |

Site and release tool versions are recorded in [09-site-and-docs.md](09-site-and-docs.md) and [10-ci-and-release.md](10-ci-and-release.md).

## 4. Spike evidence

The prototypes are throwaway code on the `spike/m1-s2-s3` branch (`spikes/s2-procid`, `spikes/s3-walk`), which is never merged. The evidence of record comes from `bench.yml` dispatched on that branch at commit `b04564c`: [100k run](https://github.com/ayagmar/kipple/actions/runs/38075346926) and [1M run](https://github.com/ayagmar/kipple/actions/runs/38075509494). Three earlier runs ([100k](https://github.com/ayagmar/kipple/actions/runs/38069110604), [1M](https://github.com/ayagmar/kipple/actions/runs/38069234733), [100k](https://github.com/ayagmar/kipple/actions/runs/38071136180)) came before the review fixes. Back then the macOS probe trusted the first mapped file unconditionally, and only `ignore` checked the file system on macOS. The warm orderings on Linux and Windows in those runs match the ones below.

Machines of record:
- **Linux runner:** `ubuntu-latest`, image `ubuntu24/20261004.327`, Linux 6.17.0-1022-azure, AMD EPYC 7763, 4 vCPUs, 15 GiB, ext4 root on a 150 GB virtual disk (`sda`, reported as rotational), run as the unprivileged `runner` user. The 38071136180 run got an Intel Xeon Platinum 8573C instead, so absolute times differ between runs.
- **macOS runner:** `macos-latest`, image `macos26/20260907.0351`, Apple M1 (virtual), 3 vCPUs, 7 GiB, APFS on SSD.
- **Windows runner:** `windows-latest`, image `win25-vs2026/20260925.250`, AMD EPYC 7763, 4 logical CPUs, 16 GiB, Microsoft virtual disk. The tree sits under `D:\a\_temp`.

### 4.1 S2: running-executable identity

Snapshot of every process from the 100k run. "Unknown" is denied, path only or other error.

| OS | Processes | Identified | Unknown | Not running | No executable | Direct probe | `sysinfo` path | `sysinfo` + `stat(path)` |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Linux runner | 166 | 7 | 159 (denied: other users) | 0 | 0 | 0.43 ms | 4.71 ms | 4.75 ms |
| macOS runner | 573 | 314 | 258 (257 path only: region info refused for other users) | 1 | 0 | 7.79 ms | 5.48 ms | 10.14 ms |
| Windows runner | 148 | 144 | 2 (`QueryFullProcessImageNameW` failed with error 31) | 0 | 2 | 16.91 ms | 7.12 ms | 10.50 ms |

Times are medians of 21 snapshots. On Linux, `sysinfo` reported `exe() == None` for all 159 denied processes, which is the same value it gives a zombie or a kernel thread. With `/proc` remounted `hidepid=invisible`, the Linux runner saw 7 processes instead of 166, and no error. Only the mount options in `/proc/self/mountinfo` show that the inventory is partial. On macOS the first mapped file matched `proc_pidpath` for every process whose region info could be read.

Scenarios, each with a copy of the spike binary started from a temp dir:

| Scenario | Linux direct | macOS direct | Windows direct | `sysinfo` + `stat(path)` |
| --- | --- | --- | --- | --- |
| File in place | identified | identified | identified | same identity |
| File deleted while running | identified, link count 0 | unknown (`proc_pidpath` fails, so the vnode can't be confirmed) | delete refused; after a rename the path follows the file, identity unchanged | Linux, macOS: path of a missing file, no identity |
| Replaced in place (new file at the same path) | identified: the old file, link count 0 | identified: the old file, link count 0 | old file renamed away first; path follows it, identity correct | Linux, macOS: the **new** file's identity |
| Exited, not reaped | exited (zombie) | gone | exited (handle still open) | Linux: `exe() == None`; macOS, Windows: not listed |
| Exited and reaped | gone | gone | exited (our handle is still open) | not listed |
| Privileged process | pid 1: denied | pid 1: path only | pid 4: no executable | Linux: `None`; macOS: a path; Windows: an empty path |

### 4.2 S3: walker and sizing

Reference trees from `cargo xtask bench-tree`: 100000 entries (16989 directories, 83011 files, 112278450 bytes) and 1000000 entries (169981 directories, 830019 files, 1102545689 bytes). Every variant counted the same entries and bytes with no errors. Every variant used all CPUs, never followed links, and checked the root's device on Linux and macOS. On Windows none of them follows junctions or mount points, and only `ignore` also checks the volume. Medians are of 5 runs. Peak RSS comes from GNU time on Linux and BSD `time -l` on macOS; there is no wrapper for it on Windows. Syscalls come from `strace -f -c` on one warm Linux run.

Linux runner:

| Variant | 100k cold | 100k warm | 1M cold | 1M warm | Peak RSS 1M warm (MiB) | Syscalls 100k / 1M |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `read_dir` + `rayon` | **1004** | **125** | **9949** | **1226** | 2.8 | 185230 / 1865363 |
| `ignore` (05 §3 settings) | 1061 | 180 | 10540 | 1800 | 4.0 | 202487 / 2024293 |
| `dua-core` | 1079 | 211 | 10643 | 2220 | 3.1 | 267806 / 2672843 |

macOS and Windows runners, warm (ms, peak RSS in MiB on macOS):

| Variant | macOS 100k | macOS 1M | Windows 100k | Windows 1M |
| --- | ---: | ---: | ---: | ---: |
| `read_dir` + `rayon` | **327** (2.2) | 11619 (2.3) | **287** | **2334** |
| `ignore` | 352 (2.9) | 11548 (3.2) | 509 | 3960 |
| `dua-core` | 376 (2.5) | **9902** (2.9) | 630 | 3853 |

The macOS runner is noisy. Its 1M samples spread by up to 38% within one variant, for example 10299 to 14234 ms for `ignore`.

Why `ignore` is slower: with `follow_links(false)` it `lstat`s every entry by full path, and `same_file_system(true)` adds a full-path `stat` of every directory. std's `DirEntry::metadata` uses `statx`/`fstatat` relative to the open directory on Linux (glibc) and macOS. Linux musl builds fall back to a full-path `lstat` in std. `dua-core` reads native bulk metadata on macOS (`getattrlistbulk`), which is why it leads there at 1M.

## 5. Open questions for the founder

**Q-M4: process evidence for a regular user.** As a regular user, Linux refuses `/proc/<pid>/exe` for other users' processes. On the CI runner 159 of 166 processes were unknown (§4.1), and `kipple doctor` on the founder's machine reported 475 of 732 unknown on 2026-10-10. Under D-035 and 04 §4 an unknown process is never idle, so any operation that needs complete process evidence, such as Codex release cleanup in M4, would always be blocked. The owner of a refused process is still visible (the owner of `/proc/<pid>`). Possible directions: keep "any unknown blocks" and ship release cleanup as report-only on Linux; count only processes of the current user and of users who could read the release store; or leave the scope to each integration's descriptor. This is the founder's decision and must be made before M4. Nothing in M1 depends on it.
