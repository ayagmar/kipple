# Functional specification

Status: partly built. Commands, flags and screens below are the target contract for v0.1 unless marked later. `kipple scan`, `rules`, `explain` and `doctor` exist since M1, read-only, as each "Today" note says.

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

### J0. First run

The first `kipple` in a TTY (no config yet) opens a short onboarding before the home screen. Every step is skippable and can be changed later (in the config file, or `,` for settings in the TUI).

1. **How careful should kipple be?** Careful, Balanced (preselected answer) or Thorough, each with one plain sentence ([04 §2a](04-safety-model.md)). The scan is already running in the background, so each option shows **live numbers for this machine**: "Balanced: 14.2 GB safe to clean, 5.1 GB to review."
2. **Where are your projects?** The detected default roots (§6), with checkboxes and "add another". Below them, unchecked, the roots kipple suggests from the projects it found elsewhere in the home directory (§6, D-053), each with how many projects it holds.
3. **Delete or move to Trash by default?** It explains that Trash doesn't free space until emptied. Saved as `[clean] method`, independent of the level.
4. **Prevention tips:** a list of native retention settings kipple found (for example a tool keeping 30 days of history). They are shown, never changed.

Without a TTY there is no onboarding: Balanced and the default roots apply, and `kipple config` prints how to change them.

Status (M2): built. It opens only when there is no config file at the default location; a file named by `--config` or `KIPPLE_CONFIG` never starts it, and an unknown config directory exits 2 before any screen opens. `enter` answers a step, `tab` skips it, `esc` goes back and `q` quits; in step 2, `space` checks or unchecks a root, and a root typed after "add another" must be absolute or start with `~` and not be in the list already, compared as kept paths are (§6). Step 1's numbers are each level's Safe and Review totals over what has been measured so far, judged exactly as `kipple scan` judges them. Step 4 lists the retention advice of the tools the scan found, or says there is none. Finishing writes everything in one go: `care.level` always (Balanced when step 1 was skipped), the project roots only when they differ from the ones found, so the defaults stay live otherwise, and `clean.method` when step 3 was answered. Quitting writes nothing, so the questions come back next time. When the roots changed, the home screen opens on a new scan.

### J1. Understand (no arguments, TTY)
`kipple` opens the TUI home. Findings stream into three **buckets** derived by host policy ([04-safety-model.md](04-safety-model.md) §2a):

```text
  Careful · Balanced · Thorough                            (level: Balanced)
  Safe to clean                                   9.9 GB   passes every check at this level
    Rust build output (target/)                   5.1 GB
    node_modules (reinstall from lockfile)        2.7 GB
    Old Codex releases (keeps active, running +1) 1.8 GB
    Python bytecode caches                        0.3 GB
  Review                                          1.3 GB   loses something, or recent
    node_modules touched this week                1.3 GB
  Protected / report-only                        11.6 GB   blocked, unknown, needs a grant, or advice only
    Agent sessions (report only)                  0.7 GB
    pacman cache  → advice: paccache -rk3         7.4 GB
```

Inside each bucket, findings are grouped by **Agents**, **Dev** and **System**. A filter switches to one lens. Totals separate *discovered*, *eligible*, *selected* and *estimated immediate reclaim* (Trash counts as zero immediate reclaim). Sizes fill in as they are computed. Known roots appear within 250 ms. Project scanning covers the configured (or default) project roots.

"Protected / report-only" shows *why* for each row: `blocked: in use`, `unknown: layout`, `needs grant`, `report only` or `advice`. These are distinct reasons, never lumped into one label.

The founder chose this bucket layout over a lens-first one in M2 (D-049).

Without a TTY, `kipple` prints a short read-only summary to stdout and never takes over the terminal.

### J2. Decide
Selecting an item opens its detail pane:
- exact path (control characters escaped), owner integration and rule ID (and pack and version for third-party rules)
- category, evidence (markers, last modified time labelled as modified, not used, git state, process state)
- what is lost and how it comes back (rebuild, re-download, gone), with cost if known
- eligibility with reasons, available methods, and any advice

Every row and detail pane shows a one-line **reason** ("Cargo build output; rebuilt by `cargo build`; project untouched for 41 days"), plus a link to the rule's docs page.

`p` **pins (keeps)** the focused item: it is excluded permanently, with an optional note, and listed under Settings → Kept items. `P` keeps the whole rule, which sets `rules.<id>.enabled = false`. Both are undone from Settings. Keeping is how users teach kipple what matters to them.

Nothing is preselected. Three ways to select:
- `s` toggles the focused item.
- `a` adds every eligible item in the focused group, in whichever bucket that group sits (Safe or Review). It can't add Protected items.
- `c` adds the whole **Safe** bucket and opens review. It never adds Review items.

Shortcuts add to the current selection and never clear it. Earlier picks stay selected and visible. Membership is frozen at the keypress, so findings that stream in afterwards don't join. Items inside the rule's effective recent window (04 §2a) are marked "recent", fall into Review, and stay out of every group shortcut, including `c`. When the level changes, nothing joins the selection: a selected item that is no longer eligible leaves it, and so does one a shortcut added that is no longer Safe or is now recent, and a line says how many left. Keeping an item or its rule takes it out of the selection. Opening review is not authorization. The confirmation in J3 is.

### J3. Act
Every deletion needs confirmation. Nothing is removed without a review screen and an explicit `y` (or a typed size above the threshold). The only exceptions are the three automation forms in J6, which the user set up explicitly.

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
- exclusions, the policy version, and each rule's **effective carefulness level, effective recent window, enabled state and effective method**

On every run:
- An entry whose rule, pack, roots, retention, method, exclusions, policy version, effective level, recent window or enabled state changed since the profile was saved is **suspended** until `kipple profile review NAME`. It is never silently re-enabled.
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

Today `doctor` reports kipple's own directories, the config file and where it came from (`--config`, `KIPPLE_CONFIG` or the default), why it can't be used if it can't (with its line and column), and a warning for each kept or excluded path that doesn't exist or can't be checked, every symbolic root of the platform with its path (for a root that is itself a link, the link and the directory it leads to), source and what a no-follow probe found there (with the OS error when it can't be read), `git`, `paccache` and `journalctl` with their versions, and how many running processes could be identified, with whether the process listing is complete. The macOS Full Disk Access hint appears only after a permission failure. It also lists each built-in integration with the roots it reads on this platform, or that it has none here. `doctor --json` writes the same as a `kipple.doctor/v1` record. Detected tool versions per integration, omissions and pack compatibility arrive with third-party packs and the M2 support status.

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

Today only these exist, and nothing changes anything:
- `--config FILE` works with every command. `kipple`, `kipple scan` and `kipple explain PATH` need a config they can use (§6) and otherwise exit 2 with the reason.
- `kipple [--ascii]` opens the home screen in a terminal (§5), or prints each bucket's total when stdout isn't a terminal.
- `kipple scan [--lens L] [--root PATH]... [--json | --events jsonl]` scans every root of the built-in integrations and reports each finding with its rule, evidence and size, then each integration's advice and omissions. `--root` replaces the default project roots. Each finding carries the level it is judged at (`level`: its rule's own, else `care.level`), its bucket at that level with every reason (`verdict`), the consequence its integration verified, and whether a running program uses it (`activity`: `idle` with how many root or `SYSTEM` processes were left out and, when any were, how many of the user's own were checked by their command line only, `in-use` with the process and only the path that matched, or `unknown`, D-050, D-052); the report counts the root and `SYSTEM` processes left out of items in the home directory (`unchecked_system_processes`) and the user's own processes checked by their command line only (`by_command_line_processes`, absent when none was), and the human report and summary print those notices; the human report shows the bucket beside each finding, with the reasons in parentheses when it isn't Safe. The configured project roots replace the defaults like `--root` does, and `--root` replaces them. An excluded path isn't walked and is listed as an omission. The Cargo, Node and Python rules propose removal, but nothing removes anything yet.
- `kipple config path` prints the config file's path and where it came from; `kipple config show` prints the settings it gives. When the config can't be used, both still print what they can and exit 2 with the reason.
- `kipple rules [--json]` lists the built-in integrations, packs and rules with their lens, roots, platforms and verified upstream versions, and the published capability set.
- `kipple explain RULE` shows what a rule selects, why, and what is lost, without reading the config; `kipple explain PATH` runs a scan and shows what was found there and the evidence for it, or what it lies inside, or, when nothing was, what the scan couldn't cover. An unknown rule ID exits 2.
- `kipple doctor [--json]` (J7).

The other global flags, `--require-complete`, `config edit` and every other command come later.

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

Status (M2, D-048, D-049): the home screen exists, removes nothing, and judges findings with the config (§6). Its header names the three levels and the current one. It has the bucket layout with Agents / Dev / System groups, the detail pane (right of the tree from 100 columns, below it otherwise), the footer totals, streaming sizes, the fuzzy filter, `tab` to show one lens, `s`, `a` and `c`, help, `g`/`G`, `NO_COLOR` and `--ascii`. A finding still being measured sits under **Measuring**, never in Safe, and can't be selected. `enter` and `c` say what is selected and that nothing was changed: review and removal come with M3. `p` and `P` keep an item or its rule, and `,` opens settings: the level, the project roots, the default method (stored only), the kept items and the rules turned off, where `d` undoes a keep or turns a rule back on. Each of these is written to the config file, and the screen changes only once the write succeeded; a write that fails says why and changes nothing. A new level re-buckets every finding in place. A root already listed, however it is spelled, is refused. A change of project roots scans again and clears the selection, because a finding's number only holds within one scan. Without a config file, the first run's questions come first (J0). The processes owned by root or SYSTEM, or running with root's rights, that weren't counted for items in your home (D-050), and the user's own processes checked by their command line only (D-052), are shown as notes after what the scan couldn't cover; a note doesn't make the scan partial. Not built yet (M2c, D-054): the project roots in the header, project grouping, the sort order, size bars, copying a path, the detail pane's scrolling and its wording for an unestimated restore cost, and the root suggestions in onboarding and settings (D-053). Later: the mouse and inline mode.

- **Layout:** Safe / Review / Protected buckets (J1) with Agents / Dev / System groups inside, a lens filter, a tree on the left with size bars, a detail pane on the right that moves below on narrow terminals, and a footer with the four totals and key hints. The header names the level, the scan state and the project roots being scanned (§6), shortened to fit as the first root and "+N"; settings lists them all.
- **Projects:** inside a bucket's group, the findings under a project root are grouped by project: the top-level directory below that root (`~/projects/api/target` belongs to `api`). A project with two or more findings in that group gets its own collapsible line with its count and total, and its findings show their path inside it (`target`, `tool/__pycache__`). A project with one finding there stays one line, `api/target`. When two projects shown share a name, both show their path from home (`~/work/api`). Findings outside every project root keep their own line. `a` on a project adds its Safe and Review findings that aren't recent, as on a group (J2).
- **Sort:** `o` cycles the order inside every group and project: size, largest first (the default); last change, oldest first; name. A project sorts by its total, its oldest finding or its name. The header names the order when it isn't size. The cursor stays on the same line when rows move.
- **Size bars:** from 100 columns, each finding has a bar of fixed width beside its size, scaled to the largest measured finding shown. A finding still being measured has none, and headings have none. `--ascii` draws it with `#`.
- **Detail pane:** what doesn't fit scrolls with `J`/`K`, and its last line says there is more. A restore cost kipple can't estimate reads "not estimated", which never changes the bucket. A project's line shows its path, its findings per bucket and their totals.
- **Keys:** `j/k` or arrows, `h/l` to collapse and expand, `/` fuzzy filter, `s` toggle, `a` add group, `c` add Safe bucket and review, `p`/`P` keep item/rule, `o` sort order, `y` copy the focused path, `J/K` scroll the detail pane, `enter` review, `?` help, `q` quit. Mouse optional. `y` sends the path to the terminal's clipboard with an OSC 52 escape, so it runs no program and works over SSH; a terminal that ignores OSC 52 copies nothing, and the message says the path was sent, not that it was copied.
- **Streaming:** rows appear as they are found. Sizes animate in with no reflow jumps (stable sort and reserved columns).
- **States:** `scanning`, `blocked` (with reason), `in use`, `recent`, `report-only`, `needs grant`, each a text label plus restrained colour. Never colour alone.
- **Empty or incomplete:** an incomplete scan is never shown as "nothing to clean". Omissions are listed.
- **Terminal hygiene:** one writer owns the terminal. Raw mode and the alternate screen are restored on exit, error and panic (an RAII guard plus a panic hook).
- **Inline mode:** `kipple clean` outside the home screen renders its review and progress inline (fixed height, scrollback kept) instead of taking over the screen. `--fullscreen` switches.
- **Accessibility:** `NO_COLOR`, `--ascii`, works at 80×24, escapes control characters in paths and plugin text.
- **Responsiveness budget:** keypress-to-frame p95 under 50 ms during a scan.

## 6. Configuration

### Where kipple keeps its own files

Every frontend uses the same directories (D-015, resolved by the platform since D-037), so the CLI, TUI and a future GUI share config, grants and receipts. These are kipple's own directories only. Each integration resolves its tool's upstream locations separately.

| | Linux | macOS | Windows |
| --- | --- | --- | --- |
| config | `$XDG_CONFIG_HOME/kipple` (default `~/.config/kipple`) | `~/.config/kipple` | `%APPDATA%\kipple\config` |
| data (receipts, packs, grants) | `$XDG_DATA_HOME/kipple` (default `~/.local/share/kipple`) | `~/.local/share/kipple` | `%APPDATA%\kipple\data` |
| cache | `$XDG_CACHE_HOME/kipple` (default `~/.cache/kipple`) | `~/.cache/kipple` | `%LOCALAPPDATA%\kipple\cache` |

On Windows, `%APPDATA%` and `%LOCALAPPDATA%` mean the `RoamingAppData` and `LocalAppData` Known Folders, read through the Known Folder API rather than the environment. A relative `XDG_*` value leaves the directory unknown, and `doctor` says so.

Config file precedence: `--config FILE`, then `KIPPLE_CONFIG`, then `<config dir>/config.toml`. Only a missing file at the default location means the defaults below. Everything else exits with code 2 and the reason, with a line and column where there is one: a named file that is missing, unreadable, a directory or a link to nothing; an unknown config directory, unless a file is named; invalid TOML; an unknown key; a key below that isn't built yet ("not supported yet"); an invalid value; or a rule ID that isn't a built-in rule. Any built-in rule can be named, whatever the platform, so a config synced from Linux with a pacman rule loads on macOS. `kipple doctor` and `kipple config path|show` report the problem instead of failing; `kipple rules` and `kipple explain RULE` don't read the config.

kipple writes the config itself from the TUI's `p`, `P` and settings, and from the first run's answers (J0). Each write is one atomic replacement: it re-reads the file at write time and applies its edits to what is there, so changes made in an editor meanwhile survive, along with comments and every other key. A config file that is a link is written where it leads, the file's permissions are kept (a new file is private to the user), and the first write creates the config directory. A file that no longer reads as a valid config, an unknown config directory, or a path that isn't valid UTF-8 refuses the write and changes nothing.

### Defaults when no config exists

- **Project roots:** whichever of these exist: `~/projects`, `~/Projects`, `~/code`, `~/src`, `~/dev`, `~/work`, `~/repos`, `~/git`, `~/Developer`, `~/source/repos`. They are shown in the TUI header as "scanning: ..." with a hint to change them. Scanning is read-only, so a broad default is harmless and makes the first run useful.
- **Suggested roots (D-053):** projects outside these roots are never scanned until the user adds a root. To help, onboarding step 2 and settings look for projects in the home directory in the background and suggest the directories that hold them. A project is a directory holding `.git` or a marker of a built-in `project-artifact` rule (`Cargo.toml`, `package.json` and the others). The search is read-only and bounded: at most 4 levels below home, no hidden directories, no links followed, no other file systems, nothing inside a project already found, the existing project roots or an excluded path, and at most 50 000 directories or 2 seconds, after which it says it stopped early. A project's parent directory is suggested, or the project itself when its parent is home; a suggestion inside another is folded into it; home itself, an existing root, a path inside one and an excluded path are never suggested. A suggestion is only shown, unchecked. Adding one writes `scan.project_roots` like "add another" does, and nothing is written otherwise. Not built yet (M2c).
- Level `balanced` (recent means modified within 7 days). No exclusions. No typed confirmation threshold.

### Example (not the defaults)

```toml
[care]
level = "balanced"            # careful | balanced | thorough

[clean]
method = "delete"             # delete | trash

[scan]
project_roots = ["~/projects", "~/work"]

[exclude]
paths = ["~/projects/keep-this/target"]
rules = ["builtin.node.node-modules"]

# Not supported yet: kipple exits 2 on these until they are built.
# [review]
# confirm_by_typing_above = "5 GB"
#
# [retention]
# "builtin.codex.stale-releases" = { keep_additional = 1 }

# Every built-in rule can be tuned or switched off.
[rules."builtin.node.node-modules"]
level = "careful"             # stricter than the global level for this rule only
recent_days = 30
# method = "trash"            # not supported yet

[keep]                        # written by `p` in the TUI
paths = [{ path = "~/projects/demo/target", note = "benchmark baseline" }]
```

### Overrides: types, limits and precedence

| Key | Type | Valid values | Notes |
| --- | --- | --- | --- |
| `care.level` | string | `careful`, `balanced`, `thorough` | global |
| `clean.method` | string | `delete`, `trash` | global default method |
| `rules.<id>.enabled` | bool | | `false` disables the rule entirely |
| `rules.<id>.level` | string | the three levels | overrides `care.level` for this rule |
| `rules.<id>.recent_days` | integer | 1–365 | can only lengthen the effective window (04 §2a): the window is the longest of the level's, the rule's and this one. A shorter value is accepted but has no effect; `doctor` doesn't warn about it yet |
| `rules.<id>.method` | string | one of the rule's `operation.methods` | not supported yet |
| `retention.<id>.*` | per rule | ≥ the rule's defaults | not supported yet; can only keep more |
| `review.confirm_by_typing_above` | size | | not supported yet |

`clean.method` is checked and stored, but nothing removes anything yet. A key listed as not supported yet exits 2 with that message.

Paths in `scan.project_roots`, `exclude.paths` and `keep.paths` are absolute or start with `~`, which means the home directory. A kept or excluded path holds a finding at it, inside it or above it (removing a parent would remove the path). Paths are compared by components, ignoring case and a trailing separator, both as written and below the directory a linked root leads to (D-041), and also by file identity, so a different spelling of the same directory still matches. When the home directory is unknown, an entry starting with `~` holds every finding as `excluded`. A path that doesn't exist or can't be checked is warned about on stderr and in `doctor`. An excluded path is not walked when looking for projects and is listed as an omission; an integration's own roots and the sizing of a finding above it still read it. A kept path is walked, and what is found there is Protected as `kept`. A rule turned off (`enabled = false` or `exclude.rules`) still runs, so its walk stops still apply, and its findings are Protected as `disabled`.

Precedence:
- **Method:** CLI `--method` > `rules.<id>.method` > `clean.method` > `delete`. A trash failure never falls back to delete.
- **Level:** `rules.<id>.level` > `care.level` > `balanced`.
- **Exclusion:** `keep`, `exclude` and `enabled = false` always win over anything that would include an item.

Unknown keys and invalid values exit with code 2. No override can relax required facts, grants, protected sets, retention protections or any invariant in 04 §3.

Exclusions are persistent and honoured by every frontend and automation. `retention` overrides can only keep *more* than a rule's default, never less than its protections.

## 7. State on disk

kipple writes only its own files:
- config
- the receipt journal (data dir)
- installed rule packs, the lockfile and grants (data dir)
- an optional scan cache (cache dir; safe to delete, and kipple reports it as its own finding)

Nothing else is written during a scan.
