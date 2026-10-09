# Functional specification

Status: planned. Commands, flags and screens below are the target contract for v0.1 unless marked later.

## 1. Vocabulary

Use these words exactly, in code, docs and UI.

| Term | Meaning |
| --- | --- |
| **Integration** | Typed built-in code that understands one tool or ecosystem (Codex, Cargo, Node...). It resolves roots, verifies layouts, supplies facts, and can construct a supported operation. Lives in `kipple-adapters`. |
| **Rule** | A declarative policy (TOML) over published selectors, facts and operations. Built-in rules use the same format as third-party ones. |
| **Rule pack** | A versioned set of rules, installed locally or from a remote source. The unit of third-party extension. |
| **Finding** | Something discovered on disk, with its owner, category, evidence, size estimate and eligibility. A finding never implies permission to delete. |
| **Category** | What a finding is: `cache`, `build-artifact`, `dependency-install`, `release-install`, `log`, `history`, `model`, `source-worktree`, `package-cache`, `trash`. For display and grouping only. |
| **Eligibility** | `allowed`, `blocked(reason)` or `unknown(reason)`, decided by host policy from facts. Never taken from a manifest's word. |
| **Method** | How an allowed operation runs: `delete` (permanent), `trash` (OS trash), `native` (the tool's own command, a semantic plan), or `none` (report-only). |
| **Selection** | The user's explicit choice of findings. Never inferred. |
| **Plan** | A frozen selection plus method, targets, fingerprints and policy digests, ready for review. |
| **Receipt** | The per-item outcome of applying a plan: `done`, `skipped(reason)`, `failed(error)`, `cancelled` or `indeterminate`. |
| **Grant** | The user's permission for a third-party rule to act on a resolved root. Installing a pack does not grant anything. |
| **Consequence** | What removing a finding costs, as composable host-derived facts: `recovery` (automatic, rebuild-local, reinstall, redownload, none), `loss` (a set from cache-warmth, rollback-ability, offline-ability, history) and `cost` (low, high, unknown). Defined in 04 §2a. Never taken from a rule's prose. |
| **Bucket** | The home-screen grouping: **Safe**, **Review** or **Protected**. Derived by host policy (04 §2a). Never declared by a rule. |
| **Profile** | A saved, frozen automation scope for `clean --yes` (§2 J6). |
| **Advice** | A copyable native setting or command (for example `sudo paccache -rk3` or Claude Code's `cleanupPeriodDays`). Shown, never run by kipple. |

## 2. Journeys

### J1. Understand (no arguments, TTY)
`kipple` opens the TUI home. Findings stream into three **buckets** derived by host policy ([04-safety-model.md](04-safety-model.md) §2a):

```text
  Safe to clean                                   5.4 GB   passes every check, rebuilds locally
    Rust build output (target/)                   5.1 GB
    Python bytecode caches                        0.3 GB
  Review                                          5.8 GB   needs a download, loses something, or recent
    node_modules (reinstall)                      2.7 GB
    Old Codex releases (keeps active, running +1) 1.8 GB
    node_modules touched this week                1.3 GB
  Protected / report-only                        11.6 GB   blocked, unknown, needs a grant, or advice only
    Agent sessions (report only)                  0.7 GB
    pacman cache  → advice: paccache -rk3         7.4 GB
```

Inside each bucket, findings are grouped by **Agents**, **Dev** and **System**. A filter switches to one lens. Totals separate *discovered*, *eligible*, *selected* and *estimated immediate reclaim* (Trash counts as zero immediate reclaim). Sizes fill in as they are computed. Known roots appear within 250 ms. Project scanning covers the configured (or default) project roots.

"Protected / report-only" shows *why* for each row: `blocked: in use`, `unknown: layout`, `needs grant`, `report only` or `advice`. These are distinct reasons, never lumped into one label.

The bucket layout is the planned design. M2 validates it against a lens-first layout with fixture data, and the founder picks.

Without a TTY, `kipple` prints a short read-only summary to stdout and never takes over the terminal.

### J2. Decide
Selecting an item opens its detail pane:
- exact path (control characters escaped), owner integration and rule ID (and pack and version for third-party rules)
- category, evidence (markers, last modified time labelled as modified, not used, git state, process state)
- what is lost and how it comes back (rebuild, re-download, gone), with cost if known
- eligibility with reasons, available methods, and any advice

Nothing is preselected. Three ways to select:
- `s` toggles the focused item.
- `a` adds every eligible item in the focused group, in whichever bucket that group sits (Safe or Review). It can't add Protected items.
- `c` adds the whole **Safe** bucket and opens review. It never adds Review items.

Shortcuts add to the current selection and never clear it. Earlier picks stay selected and visible. Membership is frozen at the keypress, so findings that stream in afterwards don't join. Items modified in the last 7 days are marked "recent", fall into Review, and stay out of group selection. Opening review is not authorization. The confirmation in J3 is.

### J3. Act
`enter` on a non-empty selection opens the review screen. It shows a summary by method ("delete 9.2 GB in 14 items, trash 400 MB in 2 items, 1 native operation"), the permanent items called out, and the exact native commands. Confirm with `y`, or type the size for plans over a configured threshold.

Apply shows per-item progress. `ctrl+c` stops new operations from being scheduled, and finished items stay in the receipt. The result screen shows the receipt totals in honest wording:

> Removed an estimated 9.2 GB of allocated storage. Moved 400 MB to Trash (still on disk). 1 item skipped: in use by process 4121 (codex).

### J4. Recover
`kipple history` lists receipts. `kipple restore <receipt> [item...]` restores trash-backed items. It refuses to overwrite, and it reports trash that was emptied or entries that are missing. Permanent, native and worktree operations say plainly that they cannot be undone.

### J5. Extend
`kipple pack add <source>` fetches a rule pack (URL with `--sha256`, or a registry name later), shows the publisher, version, digest, the roots it asks for and its rules, then installs it **without grants**. Its findings show as report-only until the user runs `kipple pack grant <pack> [rule] [--root ...]`. Updates show a diff of rules and roots and ask for new grants whenever authority grows.

### J6. Automate
Three automation forms. All of them revalidate on apply, can only lose targets, exclude ungranted third-party rules, and never let `--yes` unblock anything.

| Form | Scope | New leaves later? |
| --- | --- | --- |
| `kipple clean --only <rule-id>... --yes` | the named rules, current config | yes, within those rules |
| `kipple plan -o plan.json` then `kipple apply plan.json --yes` | exactly the frozen targets | no |
| `kipple clean --profile NAME --yes` | a saved profile | yes, within the profile's frozen scope |

**Profiles** make a cron-able "clean what's safe" possible without letting authority grow silently. `kipple profile create NAME --from-safe` shows a preview of what it would authorize, and on confirmation saves to config. A profile freezes the *effective* scope, not just names:
- rule IDs together with each rule's pack digest and the parameters that resolved them (selector, roots and their identities, retention, methods and operation)
- exclusions, and the policy version

On every run:
- An entry whose rule, pack, roots, retention, method, exclusions or policy version changed since the profile was saved is **suspended** until `kipple profile review NAME`. It is never silently re-enabled.
- New rules are listed as "pending review" and excluded.
- An item's current bucket still applies, so a once-Safe item that is now Review or unknown is skipped.

`kipple clean --yes` without `--only`, `--profile` or a plan file exits 3 and explains the three forms. There is no `--safe` flag. "Clean what's safe, unattended" is a profile.

### J7. Diagnose
`kipple doctor` explains an empty or partial scan before anyone has to guess. It reuses the same probe diagnostics as `rules` and doesn't run a second scan. It reports:
- kipple's own dirs and config source
- each integration's resolved roots and where each root came from (default, env var such as `CODEX_HOME`, or upstream setting), the detected tool version and its support status
- omissions and unreadable roots, with the OS error
- native tools present and their version (git, paccache, journalctl)
- pack compatibility
- conditional hints, such as macOS Full Disk Access, shown only when a related permission failure was actually diagnosed

Secrets are redacted. Version probes run through the platform runner with deadlines.

## 3. CLI surface

```text
kipple                         TTY: TUI home. Non-TTY: summary.
kipple scan [--lens agents|dev|system] [--root PATH]... [--json | --events jsonl]
kipple clean [--only RULE|GROUP]... [--profile NAME] [--method delete|trash] [--dry-run] [--yes]
kipple plan [--only ...] -o FILE          export a reviewed plan (automation, GUI)
kipple apply FILE [--yes]                 revalidate and apply a plan
kipple history [--json]
kipple restore RECEIPT [ITEM]...
kipple explain RULE|PATH                  why kipple thinks what it thinks
kipple rules [--json]                     integrations, rules, support status on this OS
kipple doctor [--json]                    environment and support diagnostics (read-only)
kipple profile create|review|list|remove
kipple pack add|remove|list|update|grant|revoke
kipple config path|show|edit
kipple completions SHELL
```

Global flags: `--json`, `--events jsonl`, `--no-color` (and `NO_COLOR`), `--ascii`, `-q/--quiet`, `-v/--verbose`, `--config FILE`.

There is no `clean --all --force`. `--yes` suppresses the prompt only. It never changes the method, never unblocks anything and never widens scope.

## 4. Output contracts

- **stdout carries data, stderr carries diagnostics.** Rendering never contaminates stdout in `--json` mode.
- `--json` emits one versioned envelope when the command finishes: `{"schema":"kipple.scan/v1", ...}`.
- `--events jsonl` streams versioned event envelopes. Sequence numbers follow emission order. The stream ends in a `completed` or `error` record while stdout stays writable. If stdout closes (broken pipe), kipple cancels and exits 130 without a final record.
- Final output (`--json` and human reports) is sorted deterministically (by lens, group, path), even though discovery runs concurrently. The event stream is not sorted.
- Paths in JSON: an escaped `display` string plus a lossless `raw` form (Unix bytes as base64, Windows UTF-16 as base64) when the path is not valid UTF-8.
- JSON Schemas are generated from the core types and published on the docs site.

### Exit codes

| Code | Meaning |
| ---: | --- |
| 0 | Completed. Scan finished, even if some adapters reported omissions. |
| 1 | Operation failure or partial failure, or `--require-complete` with omissions. |
| 2 | Usage, config or schema error. |
| 3 | Blocked: stale plan, revalidation removed every target, or a grant is missing. |
| 130 | Cancelled, including when stdout closes (broken pipe). |

## 5. TUI

Built with ratatui. Design goals: a calm, dense view where you always know what is selected and why.

- **Layout:** Safe / Review / Protected buckets (J1) with Agents / Dev / System groups inside, a lens filter, a tree on the left with size bars, a detail pane on the right that moves below on narrow terminals, and a footer with the four totals and key hints. M2 compares this against a lens-first layout and keeps one.
- **Keys:** `j/k` or arrows, `h/l` to collapse and expand, `/` fuzzy filter, `s` toggle, `a` add group, `c` add Safe bucket and review, `enter` review, `?` help, `q` quit. Mouse optional.
- **Streaming:** rows appear as they are found. Sizes animate in with no reflow jumps (stable sort and reserved columns).
- **States:** `scanning`, `blocked` (with reason), `in use`, `recent`, `report-only`, `needs grant`, each a text label plus restrained colour. Never colour alone.
- **Empty or incomplete:** an incomplete scan is never shown as "nothing to clean". Omissions are listed.
- **Terminal hygiene:** one writer owns the terminal. Raw mode and the alternate screen are restored on exit, error and panic (an RAII guard plus a panic hook).
- **Inline mode:** `kipple clean` outside the home screen renders its review and progress inline (fixed height, scrollback kept) instead of taking over the screen. `--fullscreen` switches.
- **Accessibility:** `NO_COLOR`, `--ascii`, works at 80×24, escapes control characters in paths and plugin text.
- **Responsiveness budget:** keypress-to-frame p95 under 50 ms during a scan.

## 6. Configuration

### Where kipple keeps its own files

Every frontend uses the same strategy (etcetera's app strategy; see D-015), so the CLI, TUI and a future GUI share config, grants and receipts. These are kipple's own directories only. Each integration resolves its tool's upstream locations separately.

| | Linux | macOS | Windows |
| --- | --- | --- | --- |
| config | `$XDG_CONFIG_HOME/kipple` (default `~/.config/kipple`) | `~/.config/kipple` | `%APPDATA%\kipple\config` |
| data (receipts, packs, grants) | `$XDG_DATA_HOME/kipple` (default `~/.local/share/kipple`) | `~/.local/share/kipple` | `%APPDATA%\kipple\data` |
| cache | `$XDG_CACHE_HOME/kipple` (default `~/.cache/kipple`) | `~/.cache/kipple` | `%LOCALAPPDATA%\kipple\cache` |

The exact Windows subpaths are confirmed against etcetera in M1 and this table is updated to match.

Config file precedence: `--config FILE`, then `KIPPLE_CONFIG`, then `<config dir>/config.toml`. A missing config file is not an error, and the defaults below apply. An invalid one exits with code 2 and a line and column.

### Defaults when no config exists

- **Project roots:** whichever of these exist: `~/projects`, `~/Projects`, `~/code`, `~/src`, `~/dev`, `~/work`, `~/repos`, `~/git`, `~/Developer`, `~/source/repos`. They are shown in the TUI header as "scanning: ..." with a hint to change them. Scanning is read-only, so a broad default is harmless and makes the first run useful.
- `recent_days = 7`. No exclusions. No typed confirmation threshold.

### Example (not the defaults)

```toml
[scan]
project_roots = ["~/projects", "~/work"]
recent_days = 7

[exclude]
paths = ["~/projects/keep-this/target"]
rules = ["builtin.node.node-modules"]

[review]
confirm_by_typing_above = "5 GB"

[retention]
"builtin.codex.stale-releases" = { keep_additional = 1 }
```

Exclusions are persistent and honoured by every frontend and automation. `retention` overrides can only keep *more* than a rule's default, never less than its protections.

## 7. State on disk

kipple writes only its own files:
- config
- the receipt journal (data dir)
- installed rule packs, the lockfile and grants (data dir)
- an optional scan cache (cache dir; safe to delete, and kipple reports it as its own finding)

Nothing else is written during a scan.
