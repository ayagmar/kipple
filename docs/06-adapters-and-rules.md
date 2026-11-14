# Integrations and rules

Status: partly built. §3 is the normative schema v1 contract, and its validator exists since M1 (`kipple_adapters::pack::validate`, tested against every error case below). The rule types live in `kipple-core`. The built-in integrations of §5a, their packs in `rules/` and the advice of §6 for pacman, journald and Claude Code exist since M1, and their findings are classified into buckets since M2. Since M4a the Codex, Claude Code, Pi, Cursor and git integrations discover and report as §5a says, and the closed logs of Claude Code and Pi, Codex's old TUI log and its plugin catalog cache are removable on Linux. Since M4b Codex's stale releases are removable on Linux too (§4), and since M4c so is a registered, present linked git worktree that passes every fact of §4, through `git worktree remove`. The other TOML examples have been parse-checked only.

## 1. Two layers

1. **Typed integrations** (Rust, first-party, in `kipple-adapters`) resolve a tool's roots, verify that its layout and version are supported, supply **facts**, and define **selectors** and **operations** that need real logic (release grouping, git worktree state, running-executable identity).
2. **Rules** (TOML, first-party and third-party) are policies over the published capability set: which selector, which predicates, which retention, which operation, what to explain.

All built-in policy ships as rules in `rules/`, in the same format third parties use. That keeps the format honest. We don't try to force discovery, interpretation and coordinated mutation into TOML. That would grow into a scripting language by accident.

## 2. Published capability set

Each kipple release publishes, in docs and through `kipple rules --json`, the capabilities rules may reference:

| Kind | Examples |
| --- | --- |
| Symbolic roots | `home`, `xdg.cache`, `xdg.data`, `xdg.state`, `known.local-app-data`, `macos.caches`, `config.project-roots`, integration roots such as `codex.release-store` |
| Generic selectors | `dirs-matching` (bounded relative glob, depth limit), `files-matching`, `project-artifact` (marker file plus artifact dir name) |
| Integration selectors | `verified-release-directories`, `registered-linked-worktrees`, `worktree-registrations` (registrations that are missing, unregistered or invalid) |
| Facts | `modified_age`, `size`, `git.tracked_changes`, `git.untracked_files`, `activity` (idle, busy, unknown), `layout` (supported, unknown), `marker.present`, `release.active`, `release.running` |
| Operations | `report` and `remove-files` (frozen leaf set) for everyone. **First-party only:** `remove-verified-release`, `git-worktree-remove`, and the `native.*` closed set. The first-party-only status comes from the capability table, not from an ID prefix. |

Every required fact that is unknown blocks eligibility. The host can restrict a rule further but never relaxes protection because a rule asks.

## 3. Rule schema v1 (normative)

### Document

There is one document format: the **pack** (TOML, UTF-8, at most 256 KiB). Built-in rules ship as packs in `rules/` (`builtin.node`, `builtin.codex`...) and use exactly the format third parties do. The examples in §4 show single `[[rule]]` tables for readability. Each one sits inside a pack document.

Unknown keys anywhere are an error. Every error carries the file, line and column, and a stable code.

### `[pack]` table (required)

| Key | Type | Required | Constraint |
| --- | --- | --- | --- |
| `schema_version` (top level) | integer | yes | `1`. Higher values give the error `unsupported-schema`. |
| `id` | string | yes | reverse-domain, `[a-z0-9-]+(\.[a-z0-9-]+)+`, at most 64 chars. The `builtin.` prefix is reserved for packs shipped in the binary. |
| `version` | string | yes | SemVer |
| `publisher`, `description`, `license`, `source` | string | yes (`source` optional for local) | at most 200 chars each, control characters rejected |
| `kipple` | string | yes | SemVer requirement on the engine version |
| `platforms` | array of `linux` / `macos` / `windows` | yes | non-empty |

### `[[rule]]` tables (1 to 64 per pack)

| Key | Type | Required | Default | Constraint |
| --- | --- | --- | --- | --- |
| `id` | string | yes | | `<pack id>.<name>`, unique |
| `integration` | string | yes | | `generic`, or a built-in integration ID. Third-party packs may only use `generic` in v1. |
| `title`, `description`, `loss` | string | `title`, `loss` yes | `description` empty | at most 200 / 1000 / 300 chars |
| `why` | string | yes | | at most 300 chars: why this exists on the machine ("Cargo writes build output here") |
| `references` | array of HTTPS URLs | no | `[]` | at most 5: upstream docs backing the rule |
| `category` | enum | yes | | one of the categories in 03 §1. Display only. |
| `platforms` | array | no | the pack's platforms | a subset of the pack's platforms |
| `scope.root` | enum | yes | | a published symbolic root. `home` is allowed only with a `pattern` of at least one literal leading component (see below). |
| `scope.selection` | enum | yes | | a published selector |
| selector parameters | per selector | per selector | | see the selector table |
| `requirements.*` | fact → required value | no | `{}` | facts the integration must report as known *and* equal to the value. Not on a `report` rule. |
| `eligibility.*` | fact → predicate | no | `{}` | see predicates. Not on a `report` rule. |
| `retention.*` | per operation | for an operation that declares retention | | see below |
| `operation.kind` | enum | yes | | a published operation. Third-party packs: `report` or `remove-files` only. |
| `operation.methods` | array of `delete` / `trash` | for `remove-files` and `remove-verified-release` | | non-empty. The host may offer fewer. |
| `selection.default` | `unselected` | no | `unselected` | the only allowed value in v1 |
| `selection.recent_days` | integer 0–365 | no | the carefulness level's window (04 §2a) | can only lengthen the level's window, never shorten it |

### Generic selectors

| Selector | Parameters | Semantics |
| --- | --- | --- |
| `dirs-matching` | `pattern` (relative glob: `*` and `?` within one component, no `**`, 1–8 components, at most 128 chars), `max_matches` (integer 1–10000, default 1000; extra matches are dropped and reported as an omission) | Matches directories whose path relative to the root equals `pattern`. The depth is the number of components in `pattern`. There is no recursive search, which keeps scans bounded. |
| `files-matching` | same as above | the same, for regular files |
| `project-artifact` | `marker` (1–8 literal file names), `artifact` (one literal directory name), `max_depth` (integer 1–12, default 6, counted in directories below each project root) | Walks the configured project roots. A directory named `artifact` whose parent contains one of the `marker` files is a candidate. The walk never descends into a candidate artifact. |

Pattern grammar: components separated by `/`. A component is 1–64 characters from `[A-Za-z0-9._@+-]` and `_` (D-043), plus the wildcards `*` and `?`. Rejected (`invalid-pattern`): absolute paths, `.` and `..` components, empty components, `**` in a component, drive or UNC prefixes, `\`, `:` (alternate streams), and NUL or control characters. Literal names (`marker`, `artifact`) use the same grammar without wildcards.

**Effective scope of `root = "home"`.** The first component must be a literal that isn't a dot-only name. The rule's effective scope, and so the most a grant can authorize, is the subtree under the longest literal prefix of the pattern. For `.bun/install/cache` that is `~/.bun/install/cache`, and for `.cache/*/tmp` it is `~/.cache`. A grant review shows this resolved subtree, never "home".

### Facts and predicates

Facts are typed. Every fact can also be `unknown`. The generic facts below are normative for v1, and third-party packs may use only these. Each typed integration publishes its own facts with their types in its descriptor, and they are listed in the generated capability catalog.

| Fact | Type | Values / unit | Applies to |
| --- | --- | --- | --- |
| `activity` | enum | `idle`, `busy` | every selector (host-supplied) |
| `modified_age` | duration | time since the newest mtime in the item | every selector |
| `size` | bytes | allocated size, or apparent size where allocated isn't available | every selector |
| `marker.present` | bool | | `project-artifact` |
| `git.tracked_files_in_item` | count | tracked files inside the matched item. **0** when the host verified that no repository encloses it or lives inside it (looked for `.git` in every ancestor from the item up to the filesystem root, every level readable, home not a boundary; and the walk that sized the item read every entry inside, met no `.git` and skipped no directory on another file system). When a `.git` lies above it, git is asked and the count is its answer (D-066); when git can't answer in full it is unknown. **unknown** when any ancestor is unreadable, the search stops early, the walk was incomplete or skipped a directory on another file system (a mount or a btrfs subvolume, whose bytes stay out of the size, so the size is a lower bound), or a repository can't be inspected. A checkout the walk met inside the item (for example a package `pip install -e` cloned into a `.venv`) holds it in Protected as `holds-repository`, even when the walk was otherwise incomplete | every selector |
| `git.tracked_changes` | count | modified tracked files in the enclosing work tree, 0 when there is verifiably no repository | every selector |
| `git.untracked_files` | count | untracked, non-ignored files in the enclosing work tree, 0 when there is verifiably no repository | every selector |

Predicates by type:

| Type | Accepted forms |
| --- | --- |
| enum | `fact = "value"` or `fact = ["a", "b"]` (one of) |
| bool | `fact = true` or `fact = false` |
| duration | `fact = { min = "30d" }`, `{ max = "1y" }` or both. Syntax `^[1-9][0-9]{0,4}(s\|m\|h\|d\|w\|y)$`, at most 100 years |
| bytes | `fact = { min = "100MB", max = "10GiB" }`. Syntax `^[0-9]{1,6}(B\|KB\|MB\|GB\|TB\|KiB\|MiB\|GiB\|TiB)$` (KB means 1000, KiB means 1024) |
| count | `fact = "absent"` (= 0), `"present"` (>= 1), or `{ min = n, max = m }` with integers 0–1,000,000 |

- A range needs at least one bound, and `min <= max`. Otherwise the error is `invalid-range`.
- A predicate whose form doesn't match the fact's type gives `wrong-type`.
- `unknown` never satisfies any predicate. That is how unknown blocks.
- The host requires `activity = "idle"` and a known `git.tracked_files_in_item` for every `remove-files` operation, whatever the rule says. Files git tracks inside the item, and their directories, are left out of what is removed, at planning and again at apply (D-066).
- `requirements` uses the same forms as `eligibility`. The engine evaluates both for every finding: a predicate that fails holds it as `fact-unmet`, and one whose fact is missing, of the wrong type or unknown holds it as `fact-unknown`. The difference is how it is worded: a requirement says the rule doesn't apply on this machine, and an eligibility predicate says what this item lacks.
- There is no negation, no `or` across facts, no arithmetic and no references between rules. A rule whose needs go beyond this is a typed integration, not a bigger language.
- A dotted fact name may be written with or without quotes: `git.tracked_changes = "absent"` is the same predicate as `"git.tracked_changes" = "absent"`.

### Validation errors

Every error has one of these stable codes plus the file, line, column and a message. `kipple pack check` exits 2 on any error.

| Code | When |
| --- | --- |
| `parse-error` | invalid TOML or UTF-8 |
| `too-large` | document over 256 KiB |
| `unsupported-schema` | `schema_version` other than 1 |
| `unknown-key` | a key the schema doesn't define, at any level |
| `missing-key` | a required key is absent |
| `wrong-type` | a value of the wrong type, or a predicate form that doesn't match the fact's type |
| `string-too-long` | a string over its limit, or containing control characters |
| `invalid-id` | a pack or rule ID that breaks the ID grammar, or a rule ID not prefixed by its pack ID |
| `duplicate-id` | the same rule ID twice in a pack, or a pack ID already installed from a different source |
| `invalid-version` | `version` isn't SemVer, or `kipple` isn't a SemVer requirement |
| `engine-incompatible` | the `kipple` requirement excludes this engine |
| `platform-mismatch` | a rule's platforms aren't a subset of the pack's |
| `too-many-rules` | more than 64 rules |
| `too-many-predicates` | more than 32 predicates in a rule |
| `invalid-pattern` | a pattern or literal name that breaks the grammar above |
| `invalid-range` | a selector parameter or predicate bound out of range, or `min > max` |
| `unknown-capability` | a root, selector, fact or operation this engine doesn't publish (the message names the engine version) |
| `not-permitted` | a third-party pack using an integration selector or fact, a first-party-only operation, or the `builtin.` prefix |

How the codes apply to the cases the table leaves open: a value outside a fixed set (a category, platform, method, enum fact value, `selection.default`) and a reference that isn't an HTTPS URL are `wrong-type`. Too many references or markers, an empty `platforms`, `methods` or enum list, and a duration over 100 years are `invalid-range`. A malformed duration or byte size is `wrong-type`. An operation that declares retention requires `[rule.retention]` (`missing-key`), and on any other operation the key is `unknown-key`. `group_by` must name `component` and `target`, once each, in either order; `keep_active`, `keep_previous`, `keep_running` and `keep_pending_install` must be `true` and `unparsable_version` must be `keep`, because the host fixes how releases group and what a group keeps, so anything else is `invalid-range`. A third-party pack naming a first-party operation or an integration fact is `not-permitted`. A rule tests the generic facts and its own integration's: a fact of another integration, or one scoped to another selector, is `unknown-capability`. So is `registered-linked-worktrees` under anything but `git-worktree-remove` or `report`, and `verified-release-directories` under anything but `remove-verified-release` or `report`. `requirements` and `eligibility` on a `report` rule are `unknown-key`, built-in packs included: a report rule selects by its selector alone, so a predicate there would decide nothing. A pack with no `[[rule]]` is `missing-key`. Errors in the `[pack]` table and in the rules are reported together whenever the pack's ID and platforms are valid.

## 4. Hard cases

### Codex stale releases (keep upstream's keep set, and never less)

On Linux `builtin.codex.stale-releases` removes the releases retention lets go, one item per release directory, through `remove-verified-release`, which plans like `remove-files`. The rule runs on Linux only: no lock kipple knows to match Codex's installer is held on macOS or Windows, so there `builtin.codex.release-inventory` reports every release but the selected one and nothing is removed. Codex prunes its own stores at each install since 0.164.0, so on a current store this rule usually finds nothing; it cleans what that prune skipped.

```toml
[[rule]]
id = "builtin.codex.stale-releases"
integration = "codex"
title = "Codex releases"
category = "release-install"
description = "Releases left after an update. The selected, previous, running and pending releases are always kept."
why = "Codex keeps the selected and previous release and prunes the rest when it installs, but skips that when it can't prove a release is unused, and never on Windows."
loss = "The removed versions. Codex downloads one again only if you install it."
platforms = ["linux"]

[rule.scope]
root = "codex.release-store"
selection = "verified-release-directories"

[rule.requirements]
layout = "supported"
inventory = "complete"
installer_activity = "idle"
"codex.version_supported" = "supported"

[rule.retention]
group_by = ["component", "target"]
keep_active = true
keep_previous = true
keep_running = true
keep_pending_install = true
unparsable_version = "keep"

[rule.operation]
kind = "remove-verified-release"
methods = ["delete", "trash"]
```

`[rule.retention]` only states what the host keeps: each key is fixed, and a rule that says otherwise is refused. There is no `keep_additional`: on Linux and macOS Codex's next install deletes whatever is kept beyond this set.

Semantics (`kipple-core/src/retention.rs`, `engine/settle.rs`):
- A group is a component (`standalone`, `app-server-daemon`) and a target. Kept in each: the release `current` selects, the `previous` that `selection-history` names, every release a process runs, and one an install is bringing in. When the history names no rollback in a group, the newest release left there stays, a prerelease ordering below its release.
- A release runs when a process's executable lies anywhere under its directory, helpers such as `codex-path/rg` included, or when a process runs a file with the identity of one of its files from elsewhere, as through a hard link. Never by name.
- What a component records answers for all of its groups. A `current` or `selection-history` that can't be relied on, an install marker (`.staging.*`, `.current.*`, `.migration-current`, `install.lock.d`, `install-lock-*`), a part of the store that can't be read, a process under `.retired.*` and a process that may run any of its releases but can't be inspected each keep every release of the component, as each stops Codex's own prune. A version that can't be ordered keeps its group, and a group with a member left unmeasured stays unsettled, which holds it.
- A release that fails §3's signature, `local-*`, `.retired.*` and `.staging.*` is never a candidate, and neither is a release whose target isn't one of the four upstream's prune considers (`{x86_64,aarch64}-unknown-linux-musl`, `{x86_64,aarch64}-apple-darwin`): a Windows release in a Linux store is only reported. A mount inside a release, at a directory or a file, a same-device bind mount included (Linux compares `statx` mount IDs), fails the signature.
- The scan takes no lock, but it looks at each one: a lock that is missing, or a link, directory or anything else but a regular file in its place, keeps every release of its component (`no-lock`), since apply could never hold it. Apply takes Codex's locks for one item without waiting and without creating them: `install.lock`, and `app-server-daemon/daemon.lock` for the daemon's store. A missing or held lock skips the item as `installer-busy`. Then it freezes the release and reads every home again in one reading through the scan's code (`Integration::revisit_group`: links, history, markers, manifests and listing). The release is settled among every entry that reading lists for its component, releases and `.retired.*` alike, under one new process snapshot. A release this run or another prune removed is no longer listed, so a run removes every stale release of a component. The release must be listed at its path with the identity the scan read, or it holds as `changed`. An entry the apply scan didn't measure as it is now (new, replaced, or listed without facts) makes its component's activity unknown, which keeps every release of it. So apply never removes what a scan at that moment keeps.

Fixtures (`crates/kipple-adapters/tests/codex.rs`, `crates/kipple-core/tests/retention.rs`, `crates/kipple-core/tests/apply/releases.rs`, `crates/kipple-platform/tests/store_lock.rs`, `crates/kipple/tests/codex_gate.rs`):
- active v3 with stopped v2 and v1 keeps v3 and v2
- active v3, running v1, stopped v2 keeps v3, v1 and v2
- the standalone and app-server-daemon groups keep their own rollbacks
- 5+3 releases with one older running (docs/11 M4 Verify): the active, running and rollback releases are kept
- a pruned store has no candidates; a rollback older than another release is kept, not the newest
- a link that changes before apply protects the new release
- two stale releases of one component go in one run; a peer removed by someone else mid-run leaves the item a candidate; a `.retired.*` entry or an `install.lock.d` that appears mid-run keeps the component
- a Windows release in a Linux store, and a file bind-mounted inside a release, are only reported
- incomplete process inventory, `hidepid`, an uninspectable Codex process, a malformed `current` and a staging directory block the group
- a held or missing install lock, and a held daemon lock, skip the release
- a missing or odd `install.lock`, and a daemon store without its `daemon.lock`, hold at scan
- a property over generated stores: settle never keeps less than a model of Codex's prune
- a property over generated stores on disk: no candidate is an entry upstream's prune keeps, judged from the entry's target, program and alias; and after a generated change between two items (an install, a flip, an entry set aside, a removal, a process list with gaps) apply never admits what a fresh scan keeps

### Git worktrees (any agent)

Worktrees are handled by a **generic Git integration** that labels which agent made each one by location alone, such as `.claude/worktrees/`, because agents of every brand leave them behind and a branch name proves nothing. In the one public report we found, 12 of 13 abandoned worktrees (13.8 GB in total) came from an agent other than Claude Code ([claude-code#26725 comment](https://github.com/anthropics/claude-code/issues/26725#issuecomment-5284720631), 2026-08-13). Claude Code's own `cleanupPeriodDays` sweep also removes *its* orphaned worktrees past that age, and kipple explains this instead of competing with it. Discovery uses `git worktree list --porcelain -z`, from git 2.36, and the facts about each worktree come from real git, with the repository config gate of 04 §6 clear before any command that reads an object.

There are four states, and each path gets exactly one finding. When several repositories list a path, as a copy of a repository does, the one git names from inside the checkout speaks for it. Every rule here is `report` today, so nothing is removed yet.

| State | Treatment |
| --- | --- |
| Registered and present | Removable on Linux only, via `builtin.git.worktree-remove`, Review at every level and only from the TUI, which requires all of: linked (not main), not locked, no submodules, registration valid both ways, repository config gate clear, no tracked changes, no untracked files, no hidden changes, no unclassified ignored files, no operation in progress, no per-worktree refs, index not locked, idle, HEAD reachable from a retained local ref, git at least 2.39.5. Uses `git worktree remove` without `--force`, never deletes branches, no undo. |
| Registered but missing | Report-only. Reclaims nothing. Shows a `git worktree prune --dry-run` advice line, because missing may mean unmounted or moved. |
| Registered but invalid | Report-only. What is on disk doesn't match the registration, or two records print one path. Review by hand; never prune, which drops the admin directory of a checkout still on disk. |
| Present but unregistered | Report-only. May hold unique commits. `git worktree repair` is advised only when the registration names a checkout that is gone. A copy of a live worktree, or a checkout that lost its repository, gets review-by-hand advice with no command. |

`node_modules`, `target` and similar directories inside any worktree are separate findings through the normal artifact rules. They can be reclaimed even when the worktree itself cannot.

```toml
[[rule]]
id = "builtin.git.worktree-anomalies"
integration = "git"
title = "Worktrees needing review"
category = "source-worktree"
why = "Agents create git worktrees and often leave them behind."
loss = "Nothing. Report only."

[rule.scope]
root = "config.project-roots"
selection = "worktree-registrations"

[rule.operation]
kind = "report"
```

## 5. v0.1 catalog

"Mutable" here depends on verifying the upstream layout and passing the native safety fixtures on each advertised OS. Anything that fails or is unknown stays blocked. The table is the v0.1 target; §5a says what is built.

Every integration descriptor declares its **protected subresources** (04 §3.9), its **upstream root precedence** (05 §1) and the **consequence facts** it can prove (04 §2a).

| Integration | v0.1 mutable | Report-only or protected |
| --- | --- | --- |
| Codex | On Linux, stale releases per component and target (no channel exists on disk); closed diagnostic logs; verified ephemeral artifacts | Histories, state DBs, plugin and runtime stores until proven disposable, credentials and config, active or unknown versions |
| Git worktrees (any agent) | Artifacts inside worktrees; strictly eligible registered linked-worktree removal (§4) | Registration anomalies (report plus `prune --dry-run` advice), dirty, locked or unreachable HEAD |
| Claude Code | Closed debug logs | Sessions, checkpoints and file history, memory, plugins, skills and config; broken or dirty worktrees; installed versions, which the installer prunes and a stale lock can't prove unused |
| Pi | Logs and ephemeral files only where upstream identifies them | Sessions, packages, extensions, cloned repos, auth and config |
| Cursor | Project artifacts under roots; tool cache and logs only if separated from history | Workspace DBs, local history, model and embedding stores, release installs |
| Cargo | Project `target/` (honouring the resolved target dir and busy evidence) | Registry and git stores (Cargo auto-GC; advice shown) |
| Node | Project `node_modules`; npm download cache via a typed native op, once its contract fixtures pass | pnpm shared store (hard links), globally installed tool state |
| Python | `.venv` with project evidence; `__pycache__` | Global interpreters, unowned venvs; `uv cache prune` later |
| Gradle, Maven, Playwright, JetBrains | Report-only inventory | Deferred until lifecycle and locking are verified |
| Pacman, AUR helpers, apt, dnf, journald | Report-only, plus native retention advice (`paccache -rk3` with a timer, `journalctl --vacuum-time`) | Never executed in v0.1 (root) |
| Ollama, LM Studio, Hugging Face | Report-only model inventory | Model removal deferred (shared blobs and manifests) |
| Trash | Report size and age | Emptying comes after 0.1.0 (`trash` crate purge with review) |

### 5a. Built today

The Cargo, Node and Python rules propose `remove-files` with `delete` or `trash`, and so do, on Linux only, the closed-log rules of Claude Code and Pi, Codex's old TUI log and its plugin catalog cache; and on Linux Codex's stale releases go through `remove-verified-release` and a registered, present linked git worktree through `git-worktree-remove`; the trash, pacman, journald and npm rules, git's other two and Cursor's only report. kipple finds, sizes and classifies each finding into its bucket (D-047), and `kipple clean` and the home screen remove only what a reviewed plan holds, through the executor. Each integration lists the upstream versions its fixtures were built from, and `kipple rules` and the generated support matrix show them. The consequence column is what the integration verified for a finding whose layout it recognised (04 §2a); an unrecognised layout and a report-only finding have none.

| Integration | Lens | Finds | Evidence | Consequence | Verified against |
| --- | --- | --- | --- | --- | --- |
| Cargo | dev | `target` beside `Cargo.toml` | Cargo's `CACHEDIR.TAG` signature, else unknown layout | `rebuild-local`, loses `cache-warmth` | Cargo 1.99.0 |
| Node | dev | `node_modules` beside `package.json` | `.package-lock.json` (npm) or `.modules.yaml` (pnpm); none for Bun | `reinstall`, loses `offline-ability` | npm 11.19.0, pnpm 12.9.1, Bun 1.4.2 |
| Python | dev | `.venv` beside `pyproject.toml`, `requirements.txt`, `setup.py`, `setup.cfg` or `Pipfile`; `__pycache__` holding only `*.pyc`, outside any virtual environment (any directory holding `pyvenv.cfg`) | `pyvenv.cfg`; the bytecode itself | `.venv`: `reinstall`, loses `offline-ability`; `__pycache__`: `automatic`, loses `cache-warmth`, cost `low` | Python 3.14.8 `venv`, uv 0.12.23 |
| Trash | system | the trash root, when not empty: on Linux the home trash's `files`, where the trashed files are (Linux, macOS) | the root | — (report only) | freedesktop Trash spec 1.0 |
| Pacman | system | the package cache, when not empty (Linux) | the root | — (report only, advice) | pacman 7.1.0 |
| journald | system | the persistent journal, when not empty (Linux) | the root | — (report only, advice) | systemd 262 |
| Claude Code | agents | on Linux, the debug logs of closed sessions; everywhere, installed versions and the rest of its store, reported only; advice | the verified install layout and no open session marker naming the log | `none` for the logs, loses `history`, Review at every level; versions: `redownload`, loses `rollback-ability` | Claude Code 2.1.318 |
| Codex | agents | the release stores: on Linux, the stale releases retention lets go (§4), and every other release but the selected one reported; elsewhere every release but the selected one; on Linux, `log/codex-tui.log` and the plugin catalog cache files; its logs and caches, reported only; advice | the selected release of a verified version in a store of a known layout | releases: `redownload`, loses `offline-ability`; the log: `none`, loses `history`, Review at every level; the cache: `automatic`, loses `cache-warmth` | Codex 0.165.0, 0.166.0 |
| Pi | agents | on Linux, four named logs of the agent directory; its store, reported only | the verified install layout and an idle installer | `none`, loses `history`, Review at every level | Pi 1.5.0 |
| Cursor | agents | on Windows, its application data, reported only; advice | none: nothing is verified, so it opens nothing | — (report only, advice) | nothing verified |
| Git worktrees | agents | each linked worktree of the repositories under a project root or an agent's location, and each checkout none of them lists, with what git says of it | the facts git gave, past the roots | on Linux a registered, present linked worktree inside a root: none, history, unknown (Review at every level); the rest report only | git 2.56.0 |
| npm | dev | nothing yet: it has its descriptor, its closed roots and its rules as `report` | — | — | nothing yet |

A `target`, `node_modules` or `.venv` with no recognised signature is still reported, with `no-signature` evidence, so an unknown layout is visible but can never become eligible. A candidate that can't be listed, including a `__pycache__`, is reported with no layout evidence at all. Generic `dirs-matching` and `files-matching` rules from third-party packs are validated but not evaluated before M5.

Roadmap integrations, each added only with layout fixtures: Gemini CLI, OpenCode, Copilot CLI, Aider, Docker and Podman (native prune as a semantic plan), Flatpak, Snap, Homebrew, Scoop, Xcode DerivedData, Android SDK and emulators, Go build cache, Bun, Deno.

## 6. Advice catalog (v0.1)

Advice is text plus an exact command or config snippet, checked against supported tool versions, and never run by kipple. An integration gives it once and each finding it applies to names it: a report-only finding that names advice proposes `advice` instead of `report`, and one that doesn't still proposes `report`.
- Claude Code (built): `cleanupPeriodDays` (default 30, minimum 1; `0` fails validation). Show the resolved value and its source, or "unknown" if it can't be determined. Explain the exceptions: memory directories, `history.jsonl` (except under a HIPAA setting), Desktop and Cowork transcripts (`desktopSessionCleanupPeriodDays`), `--bare` sessions, and versions before v2.1.228 deleting memory files. Managed, project and command-line settings override the user setting and aren't read. Source: code.claude.com settings reference and claude-directory docs, checked 2026-11-06.
- Codex (built): `history.max_bytes` caps `history.jsonl` by dropping the oldest entries. It does *not* bound session rollouts (`sessions/`, `archived_sessions/`), which have no documented retention.
- Cursor (built): `Delete Old Chats…` and `GC Agent KV Blobs` for its chat database, and `cursor.worktreeMaxCount` and `cursor.worktreeCleanupIntervalHours` for the worktrees it removes itself. kipple reports those worktrees and doesn't read Cursor's settings. Source: cursor.com docs, checked 2026-11-06.
- pacman (built): `paccache -rk N` and `paccache.timer`, with the ArchWiki rollback caveat
- journald (built): `SystemMaxUse=` or `journalctl --rotate --vacuum-size` (vacuuming alone never touches the active files), checked against systemd 262's `journalctl(1)`
- Cargo: global cache auto-GC (`cache.auto-clean-frequency`)
- git worktrees (built): `git worktree prune --dry-run --verbose` for a registration whose checkout is gone, and `git worktree repair` only when the registration names a checkout that moved; a copy of a live worktree gets review-by-hand text and no command

## 7. Integration fixtures

Fixtures are built by test code, not described in a new file format. A small `FixtureTree` builder (`tests/fixtures/tree.rs`), local to the `kipple-adapters` tests, creates directories, files with given contents, sparse files of a given size (via `set_len`) and symlinks in a temp dir:

```rust
let mut tree = FixtureTree::new();
tree.file("projects/rust-app/Cargo.toml", b"[package]\nname = \"demo\"\n")
    .file("projects/rust-app/target/CACHEDIR.TAG", CARGO_TAG)
    .sized("projects/rust-app/target/debug/demo", 4 * MIB)
    .dir("projects/rust-app/target/debug/incremental");
tree.build_in(home)?;
```

`link` and `bad_link` add real symlinks, on Windows too, where a test that can't make one fails. `link` takes what the link holds as an explicit `Holds`, since an integration can tell the forms apart: `AbsoluteInTree` is a path in the tree, held as an absolute path, and `AsWritten` is the target exactly as upstream writes it, relative to the link or absolute, in the tree or not. `modified` sets the times of the files below a directory, and `modified_dir` sets a directory's own time once its children are written. `FixtureTree::link_stamp` reads a built link's own `lstat` (Unix only), for a fixture that has to write a file recording it, since a change time can't be set.

- Fixtures live in `crates/kipple-adapters/tests/fixtures/<integration>/`, one file per supported upstream layout named after the tool and version (`node/pnpm_v12_9_1.rs`), each adding its layout to a `FixtureTree`.
- Each fixture comes with a positive case and at least one negative case (an unknown layout blocks, a running release is kept). `tests/integrations.rs` runs them through the engine.
- Supporting a new upstream version means adding a fixture file and its cases.
- `tests/fixtures/demo_home.rs` composes every layout into the one shared tree, `demo-home`, with the roots each tool would resolve there. `cargo xtask demo-home <dir>` builds it, and `cargo xtask verify-demo` scans it with the built binary and compares the deterministic `--json` report at each carefulness level (Careful and Thorough set through a `--config` file in the run's temp dir) with a per-platform `insta` snapshot in `xtask/src/snapshots/`.
