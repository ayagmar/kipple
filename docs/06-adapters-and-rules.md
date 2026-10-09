# Integrations and rules

Status: planned. §3 is the normative schema v1 contract that M1 implements. The TOML examples have been parse-checked only. No validator exists yet.

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
| Integration selectors | `verified-release-directories`, `registered-linked-worktrees`, `worktree-registrations` (every registration and every known agent worktree location) |
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
| `requirements.*` | fact → required value | no | `{}` | facts the integration must report as known *and* equal to the value |
| `eligibility.*` | fact → predicate | no | `{}` | see predicates |
| `retention.*` | per operation | no | | only for operations that declare retention |
| `operation.kind` | enum | yes | | a published operation. Third-party packs: `report` or `remove-files` only. |
| `operation.methods` | array of `delete` / `trash` | for `remove-files` | | non-empty. The host may offer fewer. |
| `selection.default` | `unselected` | no | `unselected` | the only allowed value in v1 |
| `selection.recent_days` | integer 0–365 | no | the carefulness level's window (04 §2a) | can only lengthen the level's window, never shorten it |

### Generic selectors

| Selector | Parameters | Semantics |
| --- | --- | --- |
| `dirs-matching` | `pattern` (relative glob: `*` and `?` within one component, no `**`, 1–8 components, at most 128 chars), `max_matches` (integer 1–10000, default 1000; extra matches are dropped and reported as an omission) | Matches directories whose path relative to the root equals `pattern`. The depth is the number of components in `pattern`. There is no recursive search, which keeps scans bounded. |
| `files-matching` | same as above | the same, for regular files |
| `project-artifact` | `marker` (1–8 literal file names), `artifact` (one literal directory name), `max_depth` (integer 1–12, default 6, counted in directories below each project root) | Walks the configured project roots. A directory named `artifact` whose parent contains one of the `marker` files is a candidate. The walk never descends into a candidate artifact. |

Pattern grammar: components separated by `/`. A component is 1–64 characters from `[A-Za-z0-9._@+-]` plus the wildcards `*` and `?`. Rejected (`invalid-pattern`): absolute paths, `.` and `..` components, empty components, drive or UNC prefixes, `\`, `:` (alternate streams), and NUL or control characters. Literal names (`marker`, `artifact`) use the same grammar without wildcards.

**Effective scope of `root = "home"`.** The first component must be a literal that isn't a dot-only name. The rule's effective scope, and so the most a grant can authorize, is the subtree under the longest literal prefix of the pattern. For `.bun/install/cache` that is `~/.bun/install/cache`, and for `.cache/*/tmp` it is `~/.cache`. A grant review shows this resolved subtree, never "home".

### Facts and predicates

Facts are typed. Every fact can also be `unknown`. The generic facts below are normative for v1, and third-party packs may use only these. Each typed integration publishes its own facts with their types in its descriptor, and they are listed in the generated capability catalog.

| Fact | Type | Values / unit | Applies to |
| --- | --- | --- | --- |
| `activity` | enum | `idle`, `busy` | every selector (host-supplied) |
| `modified_age` | duration | time since the newest mtime in the item | every selector |
| `size` | bytes | allocated size, or apparent size where allocated isn't available | every selector |
| `marker.present` | bool | | `project-artifact` |
| `git.tracked_files_in_item` | count | tracked files inside the matched item. **0** when the host verified that no enclosing repository exists (looked for `.git` in every ancestor from the item up to the filesystem root, every level readable; home is not a boundary). **unknown** when any ancestor is unreadable, the search stops early, or a repository can't be inspected | every selector |
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
- The host requires `activity = "idle"` and `git.tracked_files_in_item = "absent"` for every `remove-files` operation, whatever the rule says.
- `requirements` uses the same forms as `eligibility`. The difference is how a failure is reported: a failed requirement means the rule doesn't apply to this machine (an omission), and a failed eligibility predicate blocks that item with a reason.
- There is no negation, no `or` across facts, no arithmetic and no references between rules. A rule whose needs go beyond this is a typed integration, not a bigger language.

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

## 4. Hard cases

### Codex stale releases (keep active + running + pending + 1 per component/channel)

```toml
[[rule]]
id = "builtin.codex.stale-releases"
integration = "codex"
title = "Inactive Codex releases"
category = "release-install"
description = "Old Codex releases kept after updates. The active, running and one rollback release per component are always kept."
why = "Codex keeps every downloaded release after self-updating."
loss = "The removed versions; Codex re-downloads a version only if you install it again."

[rule.scope]
root = "codex.release-store"
selection = "verified-release-directories"

[rule.requirements]
layout = "supported"
inventory = "complete"
installer_activity = "idle"
process_observation = "complete"

[rule.retention]
group_by = ["component", "channel"]
keep_active = true
keep_running = true
keep_pending_install = true
keep_additional = 1
additional_order = "version-descending"
unparseable_version = "keep"

[rule.operation]
kind = "remove-verified-release"
methods = ["delete"]

[rule.selection]
default = "unselected"
```

Semantics:
- Kept per group: every active target, every release with a running executable, every pending install, plus the newest remaining release.
- A malformed `current` link blocks its group. Unknown version directories are kept.
- At apply, activity and the `current` target are re-probed, and the target set can only shrink.

Fixtures needed:
- active v3 with stopped v2 and v1 keeps v3 and v2
- active v3, running v1, stopped v2 and v0 keeps v3, v1 and v2
- the standalone and app-server-daemon groups keep their own rollbacks
- a link that changes before apply protects the new release
- incomplete process inventory blocks the group

### Git worktrees (any agent)

Worktrees are handled by a **generic Git integration** that labels which agent made each one (by location such as `.claude/worktrees/`, or by branch naming where it's verifiable), because agents of every brand leave them behind. In the one public report we found, 12 of 13 abandoned worktrees (13.8 GB in total) came from an agent other than Claude Code ([claude-code#26725 comment](https://github.com/anthropics/claude-code/issues/26725#issuecomment-5284720631), 2026-08-13). Claude Code's own `cleanupPeriodDays` sweep also removes *its* orphaned worktrees past that age, and kipple explains this instead of competing with it. Discovery uses `git worktree list --porcelain -z`.

There are three states and they get different treatment:

| State | Treatment |
| --- | --- |
| Registered and present | Removable only via `builtin.git.worktree-remove`, which requires all of: linked (not main), not locked, no submodules, registration valid both ways, no tracked changes, no untracked files, no unclassified ignored files, idle, HEAD reachable from a retained local ref. Uses `git worktree remove` without `--force`, never deletes branches, no undo. |
| Registered but missing | Report-only. Reclaims nothing. Shows a `git worktree prune` advice line, because missing may mean unmounted or moved. |
| Present but unregistered | Report-only. May hold unique commits. Repair advice. |

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

[rule.requirements]
git_inventory = "complete"
repository_identity = "verified"
storage_availability = "known"

[rule.eligibility]
registration_state = ["registered-missing", "present-unregistered"]

[rule.operation]
kind = "report"
```

## 5. v0.1 catalog

"Mutable" here depends on verifying the upstream layout and passing the native safety fixtures on each advertised OS. Anything that fails or is unknown stays blocked.

Every integration descriptor declares its **protected subresources** (04 §3.9), its **upstream root precedence** (05 §1) and the **consequence facts** it can prove (04 §2a).

| Integration | v0.1 mutable | Report-only or protected |
| --- | --- | --- |
| Codex | Inactive releases per component and channel; closed diagnostic logs; verified ephemeral artifacts | Histories, state DBs, plugin and runtime stores until proven disposable, credentials and config, active or unknown versions |
| Git worktrees (any agent) | Artifacts inside worktrees; strictly eligible registered linked-worktree removal (§4) | Registration anomalies (report plus `prune --dry-run` advice), dirty, locked or unreachable HEAD |
| Claude Code | Closed debug logs | Sessions, checkpoints and file history, memory, plugins, skills and config; broken or dirty worktrees. Version cleanup after layout verification |
| Pi | Logs and ephemeral files only where upstream identifies them | Sessions, packages, extensions, cloned repos, auth and config |
| Cursor | Project artifacts under roots; tool cache and logs only if separated from history | Workspace DBs, local history, model and embedding stores, release installs |
| Cargo | Project `target/` (honouring the resolved target dir and busy evidence) | Registry and git stores (Cargo auto-GC; advice shown) |
| Node | Project `node_modules`; npm download cache via a typed native op, once its contract fixtures pass | pnpm shared store (hard links), globally installed tool state |
| Python | `.venv` with project evidence; `__pycache__` | Global interpreters, unowned venvs; `uv cache prune` later |
| Gradle, Maven, Playwright, JetBrains | Report-only inventory | Deferred until lifecycle and locking are verified |
| Pacman, AUR helpers, apt, dnf, journald | Report-only, plus native retention advice (`paccache -rk3` with a timer, `journalctl --vacuum-time`) | Never executed in v0.1 (root) |
| Ollama, LM Studio, Hugging Face | Report-only model inventory | Model removal deferred (shared blobs and manifests) |
| Trash | Report size and age | Emptying deferred to v0.2 (`trash` crate purge with review) |

Roadmap integrations, each added only with layout fixtures: Gemini CLI, OpenCode, Copilot CLI, Aider, Docker and Podman (native prune as a semantic plan), Flatpak, Snap, Homebrew, Scoop, Xcode DerivedData, Android SDK and emulators, Go build cache, Bun, Deno.

## 6. Advice catalog (v0.1)

Advice is text plus an exact command or config snippet, checked against supported tool versions, and never run by kipple:
- Claude Code: `cleanupPeriodDays` (default 30, minimum 1; `0` fails validation). Show the resolved value and its source, or "unknown" if it can't be determined. Explain the exceptions: memory directories, `history.jsonl`, Desktop and Cowork transcripts (`desktopSessionCleanupPeriodDays`), `--bare` sessions, and versions before v2.1.228 deleting memory files. Source: code.claude.com settings reference and claude-directory docs, checked 2026-10-09.
- Codex: `history.max_bytes` caps `history.jsonl` by dropping the oldest entries. It does *not* bound session rollouts (`sessions/`, `archived_sessions/`), which have no documented retention.
- pacman: `paccache -rk N` and `paccache.timer`, with the ArchWiki rollback caveat
- journald: `SystemMaxUse=` or `journalctl --vacuum-size`
- Cargo: global cache auto-GC (`cache.auto-clean-frequency`)
- git worktrees: `git worktree prune` and `git worktree repair` for registration anomalies

## 7. Integration fixtures

Fixtures are built by test code, not described in a new file format. A small `FixtureTree` builder, local to the `kipple-adapters` tests, creates directories, files (sparse via `set_len` for large sizes), symlinks and mtimes in a temp dir:

```rust
let home = FixtureTree::new()
    .dir(".codex/packages/standalone/releases/0.160.1-x86_64-unknown-linux-musl")
    .file(".codex/packages/standalone/releases/0.160.1-x86_64-unknown-linux-musl/codex", 280 * MB)
    .symlink(".codex/packages/standalone/current", "releases/0.161.0-x86_64-unknown-linux-musl")
    .build()?;
```

- Fixtures live in `crates/kipple-adapters/tests/fixtures/<integration>/v<upstream-version>.rs`, one per supported upstream layout.
- Each fixture comes with a positive case and at least one negative case (an unknown layout blocks, a running release is kept).
- Expected findings, eligibility and retained sets are `insta` snapshots of the deterministic `--json` report.
- Supporting a new upstream version means adding a fixture file and snapshots.
- `fixtures/demo-home` at the repo root is the one shared tree, used by the end-to-end tests, the docs demo and the tapes. It is built by `cargo xtask demo-home <dir>` from the same builder.
