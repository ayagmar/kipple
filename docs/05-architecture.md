# Architecture

Status: partly built. The core ports, `Engine::scan` with its events and the shared project walk (§2, §3), the platform's root resolution, probes, walker and capability probes (§1, §3), the built-in integrations, and the `scan`, `rules`, `explain` and `doctor` commands exist since M1. The Rust in §2 shows the signatures as they are, and items marked *planned* are sketches that have not been compiled. Nothing is frozen yet.

## 1. Workspace

```text
kipple/
├── crates/
│   ├── kipple-core/       domain types, ports (traits), policy, engine, events, receipts
│   ├── kipple-platform/   OS facts, process probes, confined fs executor, trash, journal store
│   ├── kipple-adapters/   typed integrations + rule (TOML) loader/validator + pack install/lock
│   └── kipple/            binary: clap CLI, ratatui TUI, composition root
├── xtask/                 dev-only task runner (`cargo xtask`), never shipped
├── rules/                 built-in rule packs (same format as third-party)
│                          the shared demo tree is built by `cargo xtask demo-home <dir>` from
│                          crates/kipple-adapters/tests/fixtures/ (not committed as a tree)
├── site/                  Astro landing page and Starlight docs
└── docs/                  this specification
```

There are four crates, and no fifth (no testkit, no SDK) until reuse is demonstrated. We don't create a crate per integration or per OS.

### Dependency direction

```text
kipple (bin) ──► kipple-adapters ──► kipple-core
      │                                   ▲
      └────────► kipple-platform ─────────┘
```

- `kipple-core` depends on no other kipple crate and has no I/O of its own. It owns the traits that platform and adapters implement.
- `kipple-adapters` and `kipple-platform` depend only on core. They don't depend on each other.
- The binary is the only composition root. It wires platform implementations into the engine and registers integrations.
- A future GUI (Tauri or native) is a second composition root that depends on the same three library crates.

These boundaries are enforced by Cargo itself (no dependency means no access) plus a CI check on `cargo metadata` that rejects new kipple-to-kipple edges.

### OS-specific code

**Capabilities, not distro names.** The platform reports *capabilities* that pass a read-only probe, for example `PackageCache::Pacman { paccache: Version }`, `Journal::Systemd`, `Container::Podman`. Presence of an executable alone isn't enough: the version must be supported and the probe must succeed. Policy never branches on `/etc/os-release` IDs. Those appear only in `doctor` output. Only providers kipple actually supports are detected.

**Upstream configuration first.** Each integration resolves its tool's roots using that tool's documented precedence: `CODEX_HOME`, `CLAUDE_CONFIG_DIR`, Pi's `--session-dir` / `PI_CODING_AGENT_SESSION_DIR` / `sessionDir` setting, and so on. Defaults come second. The composition root captures one environment snapshot and passes it through `ScanContext`. An explicit override that is malformed or unsupported makes the integration report unknown; it never falls back silently to the default. kipple can't see the flags another process was started with, so integrations report which sources they used, and `doctor` says that discovery may be incomplete. Users can add extra roots in config.

`#[cfg(target_os)]` appears only in `kipple-platform`: its `src/os.rs` picks one of `src/os/{linux,macos,windows}.rs` (`src/os/unix.rs` holds what Linux and macOS share), and a few platform tests are gated to one OS. Core and adapters are platform-neutral. Integrations ask the platform for symbolic roots (`SymbolicRoot::XdgCache`, `SymbolicRoot::LocalAppData`, and tool roots such as Codex's later), never for hard-coded paths.

**Roots today.** `resolve_roots` turns one environment snapshot into every symbolic root the OS has, each with its source: `home` from `HOME` and the XDG roots from their variables or their `$HOME` defaults on Linux and macOS, `macos.caches` (`~/Library/Caches`), and on Windows `home`, `known.local-app-data` and `known.roaming-app-data` from the Known Folder API. A relative `XDG_*` value leaves its root unknown. Tool roots follow: `config.project-roots` (the defaults of 03 §6 that exist, each directory once even where the file system ignores case, or the `--root` paths instead, else the config's `scan.project_roots`; a default that can't be checked, or is a link, is kept so opening it reports what is wrong; each `--root` is placed in the directory its parent names, the parent's links and `..` resolved (on Windows in the usual `C:\` or `\\server\share` form, not the `\\?\` one), and keeps its own name, so a `--root` that is itself a link is resolved once like any root; one that doesn't exist, or whose parent can't be resolved, is unknown), `claude.config` (`CLAUDE_CONFIG_DIR`, default `~/.claude`), `trash` (`$XDG_DATA_HOME/Trash/files` on Linux, where the home trash keeps the trashed files, `~/.Trash` on macOS; none on Windows yet), and on Linux `pacman.cache` (every `CacheDir` in `/etc/pacman.conf`'s `[options]`, default `/var/cache/pacman/pkg`; an `Include` there leaves it unknown, since the setting may be in the included file) and `journald.journal` (`/var/log/journal`). Those fixed system paths are read below the environment's system root, `/` unless a test moves it with `Environment::with_system_root`, and a `CacheDir` with `..` is resolved like a `--root`. Each root keeps a path it resolved to only once; when the same directory is reached directly and through a link, the one kept carries the link's identity check, whichever came first. With the `test-roots` feature, `KIPPLE_TEST_ROOTS=<file>` replaces every root with the `name = path` lines of that file, for end-to-end tests: a `now = <seconds since the Unix epoch>` line there fixes the clock, and while the file is in use the process table is empty and complete, so a test reads no real process. Release builds don't have it. kipple's own directories hang off the same roots (D-037). A root whose own path is a link is resolved once, there (D-041): the root becomes the directory the link chain leads to, with the configured path and that directory's identity kept in its `RootLink`. Only the root's own path is resolved. A chain that can't be followed, or that leads to something other than a directory, leaves the root unknown.

**Capabilities today.** `probe_tools` finds `git`, `paccache` and `journalctl` on the absolute entries of `PATH` and runs `--version` through the platform runner (D-038). A tool counts only with a parsed version. Which versions an integration supports is decided with that integration.

## 2. Core contracts

### Ports (implemented by `kipple-platform`, faked in tests)

```rust
pub trait FsProbe: Debug + Send + Sync {   // read-only, no-follow
    fn metadata(&self, path: &Path) -> Result<EntryMeta, ProbeError>;          // kind, identity
    fn read_dir(&self, dir: &Path) -> Result<Vec<DirEntryMeta>, ProbeError>;   // all or nothing
    fn open_dir(&self, dir: &Path) -> Result<(), ProbeError>;                    // listable, without listing
    fn read_file(&self, path: &Path, limit: u64) -> Result<Vec<u8>, ProbeError>; // regular file only, bounded
}

pub trait Sizer: Debug + Send + Sync {     // the walker (D-036)
    fn size(&self, path: &Path, cancel: &CancelToken) -> Result<Measurement, ProbeError>;
}                                          // size, plus the newest modification time inside

pub trait ProcessProbe: Debug + Send + Sync {
    fn snapshot(&self) -> Result<ProcessSnapshot, ProbeError>;  // one outcome per process (D-035)
}

pub trait Clock: Debug + Send + Sync { fn now(&self) -> SystemTime; }

pub struct Ports { pub fs, pub sizer, pub processes, pub clock }   // boxed, what Engine::new takes

// planned, with their first consumers:
pub trait Executor: Send + Sync {          // the only mutation boundary (M3)
    fn execute(&self, op: &AuthorizedOp, journal: &mut dyn Journal) -> ItemOutcome;
}

```

The probes are path-based. Spike S1 decides whether read-only probes become handle-relative like the executor.

`AuthorizedOp` has private constructors. Only the engine's policy code can build one, from a validated plan entry and a user authorization. Integrations never see `Executor`.

### Integrations (implemented in `kipple-adapters`)

```rust
pub trait Integration: Debug + Send + Sync {
    fn descriptor(&self) -> IntegrationDescriptor;   // id, lens, platforms, roots, selectors, artifacts, owner files, verified versions
    fn discover(&self, cx: &ScanContext<'_>, sink: &mut FindingSink<'_>) -> Result<(), IntegrationError>;
    // planned:
    // fn facts(&self, cx: &ScanContext<'_>, finding: &Finding) -> Result<FactSet, IntegrationError>;
    // fn propose(&self, finding: &Finding, facts: &FactSet) -> Vec<ProposedOp>;  // data only
}
```

`Engine::new` takes the ports, the integrations and the validated rules, and gives each integration its own rules; a rule whose integration isn't registered is refused. An integration reads the roots its descriptor declares plus its rules' roots. `ScanContext::project_artifacts(rule)` returns a `project-artifact` rule's candidates from the shared walk (§3), `walk_stops(root)` everything that walk stopped at, and `read_file` a bounded, no-follow read for evidence such as Cargo's `CACHEDIR.TAG`. `FindingSink::add(rule, path, evidence, consequence)` records a finding with its rule, category, `Evidence` (`Marker`, `Signature`, `NoSignature` or `Root`) and, when the integration verified what its tool wrote there, the `Consequence` of removing it (04 §2a), and `advise` records `Advice`, which the report carries per integration and which kipple never runs.

The engine opens each declared root without following links before `discover` runs. A root that doesn't exist is simply absent. One that is unresolved, unreadable (including a directory that can't be listed) or not a directory becomes an omission and is not handed to the integration, and so does a root resolved through a link whose directory is gone or no longer has the identity recorded when it was resolved. `ScanContext` exposes the roots that opened, read-only probes confined to them, and cancellation. A path outside every root, with a `..` component, or through a link below its root is refused. A trailing separator or `.` component is dropped first, since either would make the OS follow a link in the last component. `FindingSink::add` applies the same confinement, streams each finding as `FindingAdded` and fails once the scan stops, so the integration returns early. If `discover` panics, what it found until then is kept and the integration gets a `Crashed` omission. These checks are path-based until spike S1's handle-relative probes, so a link swapped in between check and use is not yet caught. Rules (TOML) are evaluated by the core against an integration's published selectors and facts. See [06-adapters-and-rules.md](06-adapters-and-rules.md).

### Engine API (what every frontend calls)

```rust
impl Engine {
    pub fn scan(&self, req: &ScanRequest, events: &EventTx, cancel: &CancelToken) -> ScanReport;
    // planned:
    pub fn plan(&self, report: &ScanReport, sel: Selection, method: MethodChoice) -> Result<Plan, PlanError>;
    pub fn apply(&self, plan: &Plan, auth: Authorization, events: &EventTx, cancel: &CancelToken) -> Receipt;
    pub fn restore(&self, receipt: &ReceiptId, items: ItemSelection, events: &EventTx, cancel: &CancelToken) -> RestoreReceipt;
    pub fn history(&self) -> Result<Vec<ReceiptSummary>, StoreError>;
}
```

These calls are synchronous and blocking, run on worker threads, and report through a bounded event channel. `scan` runs every integration on its own thread, then sizes each integration's findings through the `Sizer`. There is no async runtime in core. Filesystem work is blocking, and a bounded thread pool is enough. Async may appear later in the binary for pack downloads only.

### Buckets

When the engine measures a finding it also establishes its `Facts` (D-047): what its rule proposes (`report`, `advice` when the integration gave advice, or `remove` with its methods), whether the integration recognised the layout (a `Signature`), the consequence it verified, whether a git repository could track files inside it (`.git` looked for in the item and every directory above it through the `FsProbe`, and inside it by the walker that sizes it), whether a running program uses it (one process snapshot per scan, D-035), and how long ago anything inside it last changed (the walker's newest modification time against the `Clock`). It also records the file identity of the finding and of each directory above it (`Lineage`), for matching the config's paths. `Policy::verdict(rule, path, facts)`, reached through `Finding::verdict(&Policy)`, is the one pure function that turns them into a `Verdict`: `Safe`, `Review` with every `Caution`, or `Protected` with every `Hold` (04 §2a). Frontends call nothing else, so the CLI, the TUI and a future GUI agree, and a frontend can compare levels without scanning again. `Level` has `Careful`, `Balanced` and `Thorough` with 04 §2a's tables. A `Policy` holds what the config decides (D-051): the global level, each rule's own level, recent window and whether it is turned off, and the kept and excluded paths (`HeldPath`, matched at, inside or above by path and by identity). The config's holds come before any fact, at every level. The binary builds the `Policy` from the config file; the core reads no file.

### Key types

- **IDs:** `IntegrationId`, `FindingId`, `RuleId` and `PackId` exist; `PlanId` and `ReceiptId` are planned. All are distinct newtypes. IDs are references, never authorization. A `FindingId` is unique within one scan.
- **`Finding`:** today its ID, integration, rule, category, path, evidence, size and `Facts`. Planned: pack and version in the owner, `PathRef` (lossless OS path plus escaped display), `SpaceEstimate`, loss and recovery description, eligibility with reasons, allowed methods, advice.
- **`SpaceEstimate`:** apparent, allocated, unique-reclaim estimate, completeness (`Complete`, `Incomplete { unreadable, skipped_mounts }` or `Cancelled`). A directory inside on another file system (a mount or a btrfs subvolume) isn't entered and makes the estimate incomplete. A hard-linked file counts once, and one with a link outside the item is left out of the unique-reclaim estimate. Windows reports neither allocation nor link counts cheaply, so there the last two are unknown.
- **`Plan`:** schema version, creation time, entries (target identities and fingerprints, method, operation), policy, exclusion and pack digests, and a review DTO for display. Imported plan files are untrusted input: they are re-planned and revalidated, never executed as written.
- **`Receipt`:** per-item `Done | Skipped(Reason) | Failed(ErrorDetail) | Cancelled | Indeterminate`, method, recovery handle where one exists, estimated bytes, and an optional observed free-space change.
- **Errors:** `thiserror` enums with structured codes and context (integration, item, OS error or tool exit code and stderr). Upstream detail is preserved in the error value and shown to the user. Only structured fields are persisted (04 §8a). Strings never become the public API.

### Events

```text
ScanStarted, RootOpened, FindingAdded, FindingUpdated(size, facts),
IntegrationCompleted{omissions}, ScanCompleted
planned: IntegrationProgress, PlanReady, ItemStarted, ItemCompleted(outcome), ApplyCompleted
```

- **Bounded and cancellation-aware.** Producers send with a short timeout in a loop that checks the cancel token, so a stalled consumer can never stop cancellation from being acknowledged. Progress events may be coalesced. Findings and outcomes are never dropped. They wait, and the scan slows down rather than losing data.
- **Consumer gone.** Dropping the receiver before the stream ended cancels the scan, even while a walk is in progress and nothing is being sent. No new work is scheduled, and the call returns its partial report or receipt.
- **Closing records after a cancel.** Once cancelled, `IntegrationCompleted` and `ScanCompleted` are sent only if the stream has room at that moment, so a stalled consumer can't hold up the acknowledgement. The returned report always carries the outcome and every omission.
- **Broken pipe.** In the CLI, a closed stdout (`kipple scan --events jsonl | head`) counts as cancellation. It exits with 130 and prints nothing more to stderr. A final JSONL record is promised only while stdout stays writable.
- **Events carry data.** `FindingAdded` carries the whole finding, unsized. `kipple scan --events jsonl` writes each event as a `kipple.event/v1` record and ends with a `completed` record holding the sorted report.
- **Ordering.** Event sequence numbers follow emission order: the receiving end numbers events from 0 as they arrive. Only the final `ScanReport` and `--json` output are sorted deterministically. The stream is not.
- **Cancellation budget.** "Within 100 ms" means scheduling stops and the cancel is acknowledged, under local I/O that we control. It doesn't cover an OS call that is already blocked and can't be interrupted (for example a hung network mount).
- Tests cover a full queue with a stalled consumer, a slow consumer that still gets every finding, a consumer dropped while findings stream and while a walk runs, and cancellation during sizing.

## 3. Performance design

- **Targeted first.** Known integration roots are probed directly: a handful of `stat` and `read_dir` calls each. The first findings appear in under 250 ms.
- **One walk per root.** Each root a `project-artifact` rule or an integration asks for is walked once per scan, by whichever integration asks first, while the others wait for it. The walk lists directories with the no-follow probe, never descends into a link, and stops at every candidate artifact of any rule (its name with one of that rule's markers beside it) and at every directory a registered integration always owns (`node_modules`, `.venv`, `__pycache__`). It doesn't look inside a directory holding a file that makes it a tool's own, such as a virtual environment's `pyvenv.cfg`, whatever it is called. A directory reached through two paths of the same root, given twice or one inside the other, is found once. It goes as deep as the deepest rule's `max_depth`. A directory below the root it can't list becomes an `Unlisted` omission of each integration that used the walk. An integration's project findings arrive once its walk of that root is done.
- **Walker** (our own, in `kipple-platform`; spike S3, D-036). std `read_dir`, with metadata taken from each `DirEntry` without following links, and one task per directory on the bounded `rayon` pool. It reads no ignore file or git config and skips nothing hidden. It never follows symlinks or junctions and stays on the root's file system. It sizes findings. The project walk that finds them is the core's shared walk above, over the `FsProbe`, which honours `max_depth` and stops at candidate artifacts. A directory matching the config's `exclude.paths` (`ScanRequest.exclude`, by path or by identity) is not descended and becomes an `Excluded` omission. That prunes only the project walk: an integration's own roots and the walker sizing a finding above an excluded path still read it, and such a finding is held as `excluded`. Fixtures cover gitignored `target/` and `node_modules`, hidden `.venv`, and `.claude/worktrees`.
- **Bounded parallelism.** Size computation runs on a work-stealing pool, not unbounded: one thread per CPU today, to be tuned to the storage. It can be cancelled at directory granularity.
- **Prune early.** Discovery skips into a recognised artifact directory only for sizing, never to look for nested projects inside `node_modules` or `target`.
- **TUI decoupling.** The UI thread renders from a snapshot model updated by events at most once per frame (60 Hz cap). Large leaf sets are frozen inside the engine, not kept as UI rows.
- **Optional scan cache.** Directory identity plus mtime keyed sizes in the cache dir. It is only a hint, and a cold scan must still meet the budgets.
- **Budgets** (to calibrate against fixtures, not marketing claims):
  - known-root results in under 250 ms
  - keypress-to-frame p95 under 50 ms while scanning
  - cancellation acknowledged within 100 ms
  - fixture benchmarks at 100k and 1M entries, cold and warm, tracking time, RSS and syscalls

## 4. Frontends

- **CLI** (clap): parses arguments, builds requests, renders reports, events or JSON. No policy.
- **TUI** (ratatui and crossterm, in `crates/kipple/src/tui/`): a state machine, `App::update(Msg)` over keys, resizes and scan events, plus a pure `render(&App, Look, &mut Frame)`. Effects happen at the edge: `tui::run` starts the scan on its own thread and drives one loop that reads a key (waiting at most one 16 ms frame while the scan runs and 250 ms once it has ended), applies at most 256 scan events, and draws at once after a key and at most once a frame otherwise. It is generic over the ratatui backend and the key source, so `cargo xtask bench-tui` runs the same loop on a test terminal (D-048). The terminal is restored on quit, error and panic.
- **GUI** (later): links the library crates directly and subscribes to the same event stream. It does not shell out to the CLI or parse its JSON. Other languages can use `kipple --events jsonl` as a stable protocol.

## 5. Persistence

| Store | Location | Format |
| --- | --- | --- |
| Config | platform config dir | TOML |
| Receipts journal | platform data dir `receipts/` | append-only JSONL per receipt, intent before effect |
| Packs | data dir `packs/<id>/<version>/` | exact manifest bytes |
| Lock and grants | data dir `packs.lock` | TOML with digests, sources and grants |
| Scan cache | platform cache dir | versioned binary, safe to delete |

Only `kipple-platform` reads and writes the config file (`read_config`, `write_config`). The read takes a regular UTF-8 file of at most 1 MiB, and refuses a directory or a link to nothing. The write is the one atomic replacement of 03 §6: a `create_new` temp file beside the file (beside its target when it is a link), written, synced, renamed over it, then the directory synced. The binary parses and edits the TOML with `toml_edit`, so comments and unknown formatting survive, and resolves `scan.project_roots` like `--root` (`resolve_roots_from`).

Every on-disk format carries a schema version. A newer unknown version is read-only, or blocks with an actionable error.

## 6. Testing seams

- Core policy and planning are unit-tested with fake ports and built `FactSet`s. No disk is involved.
- Integrations are tested against `FixtureTree` layouts in `crates/kipple-adapters/tests/fixtures/` (see 06 §7), through the engine over a small std-based probe.
- The platform executor and trash get disposable integration tests on each native OS in CI.
- The CLI gets end-to-end tests on `demo-home` with every root redirected by the `test-roots` override: `kipple::run` in-process for the event stream, broken pipe and `explain`, and the built binary's `scan --json` against a per-platform `insta` snapshot in `cargo xtask verify-demo`.
- The TUI gets state-transition tests and `insta` snapshots of rendered buffers at 80×24 and 160×48, fed with scan events built in the test (`crates/kipple/src/tui/app/tests.rs`).

See [08-engineering-standards.md](08-engineering-standards.md) for what not to test.
