# Safety model

Status: mostly planned. The bucket classification of §2a is built at all three levels (D-047), with the facts listed there; every frontend judges with the configured level and per-rule settings, and the `kept`, `excluded` and `disabled` holds of the config (D-051). The core's planner, revalidation, apply loop and reconciliation are built against the mutation ports (M3a), holding §3.2's home and file-system-root protections, §3.3, §3.5, §3.6, §3.10 and §7. The platform's freezer and confined executor (§3.4–3.5, D-061) are built and have native tests for §10's confinement scenarios on Linux, macOS and Windows, and so is the trash backend of §5 (D-062). No command uses any of this yet, so nothing user-facing deletes. The rest are release gates. An operation that cannot meet them on a given OS or filesystem stays report-only there, with the limitation published. A failing integration doesn't block releasing the integrations that pass.

## 1. Who holds authority

**Integrations and rules explain and propose. The core decides and executes. Frontends review and authorize.**

- Integrations get read-only probes (a `ScanContext`). They never receive a handle that can mutate.
- Rules choose from published selectors, facts and operations. They cannot add facts, run commands or claim safety.
- The platform executor is the only code that mutates the filesystem or runs native commands.
- Frontends (CLI, TUI, GUI) show the plan the core produces and pass the user's authorization back. None of them builds operations itself.

## 2. Four separate concepts

| Concept | Decided by | Never decided by |
| --- | --- | --- |
| Category (what it is) | integration or rule | — |
| Eligibility (may it be touched) | host policy from facts | a manifest's category or wording |
| Method (how) | host policy, then user choice among the allowed methods | `--yes` |
| Selection (which items) | the user | defaults, groups, new versions |

## 2a. Buckets and consequences

Buckets are derived by host policy for each finding. Rules never declare them.

Consequence facts, set by the host:
- `recovery`: `automatic` (the tool regenerates it on use), `rebuild-local` (a local rebuild with no network), `reinstall` (the package manager may download), `redownload`, or `none`
- `loss`: a set drawn from `cache-warmth`, `rollback-ability`, `offline-ability` and `history`, possibly empty
- `cost`: `low`, `high` or `unknown`. An integration sets `high` when it knows restoring is expensive (for example a very large build)

Classification is **ordered and exhaustive**. The first matching row wins:

| # | Bucket | Condition |
| ---: | --- | --- |
| 1 | **Protected / report-only** | not eligible: blocked, unknown required fact, needs a grant, report-only operation, or advice only. The specific reason is always shown |
| 2 | **Safe** | eligible, *and* `recovery` and `loss` are within the user's **carefulness level** (below), *and* `cost` isn't `high`, *and* the item isn't recent for that level, *and* the method is available and granted, *and* there is no conflict with another operation on the same tree |
| 3 | **Review** | every other eligible finding: outside the level, `none` recovery, `history` loss, high cost, recent, or in conflict |

An unknown `cost` is optional. It doesn't block Safe, and the detail pane and `kipple explain` say the restore cost is "not estimated".

Status: the core's planner evaluates row 2's other conditions before an item can be planned (M3a), and is stricter than the table: an item whose method its rule doesn't offer, whose file system kipple doesn't change, or whose method is trash where no trash can take it, isn't planned, and neither is one from a third-party rule, since nothing grants those yet. Two selected items in conflict (one at or above the other, or one directory selected through two rules with two methods or scopes) are both refused until the review leaves one out, rather than planned as Review. No command plans or applies yet (M3b).

### Carefulness levels

The level moves the line between **Safe** and **Review**. It never moves anything out of **Protected**, and never touches an invariant in §3. A level is set during onboarding, changed any time in config or the TUI settings screen, and can be overridden per rule.

| Level | Safe may include `recovery` | Safe may include `loss` | Recent window |
| --- | --- | --- | --- |
| **Careful** | `automatic`, `rebuild-local` | `cache-warmth` | 14 days |
| **Balanced** (default) | + `reinstall`, `redownload` | + `offline-ability` | 7 days |
| **Thorough** | same as Balanced | + `rollback-ability` | 2 days |

Levels change classification only. The removal method is a separate setting (03 §6).

**Effective recent window** for a rule = the largest of: its level's window, the manifest's `selection.recent_days`, and the config override `rules.<id>.recent_days`. Overrides can only lengthen it. This one value drives the bucket, the "recent" label and every group shortcut.

Never Safe at any level: `history` loss, `none` recovery, `high` cost, third-party rules (recovery `none`), anything unknown or blocked. Retention protections (keep active, keep running, keep N) apply at every level. Thorough doesn't lower `keep_additional`; it only lets that retained-rollback loss count as Safe.

What this means in practice:
- **Careful:** only build output and regenerated caches.
- **Balanced:** adds `node_modules` and venvs, download caches, and inactive agent releases beyond the kept rollback.
- **Thorough:** adds things that cost you a rollback, for example package-cache entries beyond the kept versions once privileged operations exist.

**Useful by default.** Balanced must put the bulk of regenerable bytes in Safe. The M2 acceptance includes a fixture assertion: on `demo-home`, at least 80% of the bytes the integrations mark rebuildable, reinstallable or re-downloadable are Safe under Balanced, counting the findings whose activity the OS lets kipple judge: on Windows in M2, `node_modules`, `.venv` and `__pycache__` are always Protected (§4), so only the rest counts there. If careful defaults leave nothing to clean, that's a product failure, not a safety win.

- **Consequence is composable.** `loss` is a set, and `recovery` and `cost` are separate facts. A package cache prune loses rollback and offline installs at once.
- **Consequence facts come from the host.** For typed integrations they come from verified semantics. For third-party `remove-files` rules, recovery is `none`, so by row 3 they always land in Review (or Protected) and can never be Safe. A pack's `loss` prose is display text, never input to classification.
- **Unknown is never Review.** Missing activity or required facts block mutation. More confirmation can't make up for missing evidence.
- **Optional facts don't gate.** An unknown optional fact, such as a restore cost kipple can't estimate (shown as "not estimated"), doesn't make the item Safe, and it doesn't block it.
- **Native doesn't mean disposable.** A tool's own command constrains *how* something is removed, not *what is lost*. Native operations go through the same ordered table. For example, a package cache prune loses `rollback-ability` and `offline-ability`, so it is Safe only at Thorough. A journal vacuum loses `history` (diagnostics), so it is never Safe.
- **"Safe" means passing the current checks**, not "risk-free". The detail pane lists residual risks (§11).

## 3. Invariants

1. **Scan is read-only** with respect to discovered data: no VACUUM, no lockfiles, no native maintenance, no network. kipple writes only its own state.
2. **Protected sets:** filesystem roots, `$HOME` itself, config and credential stores, VCS sources (tracked files), and tool data whose semantics are not modelled. Protection comes from ownership semantics, not from a filename blocklist alone.
   - **One narrow exception:** the first-party `git-worktree-remove` operation may remove a *linked* worktree's checkout (tracked files included) through `git worktree remove` without `--force`. All of the facts in [06-adapters-and-rules.md](06-adapters-and-rules.md) §4 must be satisfied. The operation is bound to the verified repository and registration identity, it gets its own review line marked "cannot be undone", and it is never available to third-party packs. Tracked-file protection holds everywhere else, including inside artifact directories.
3. **Unknown means blocked.** This covers an unsupported layout or schema version, unknown ownership, an unreadable subtree, an uncertain busy state, and a changed fingerprint, policy or pack digest. If process observation fails, that is never read as "idle".
4. **Confinement.** Operations stay inside an approved root capability. Traversal never follows symlinks or junctions. Root identity (device and inode, or the Windows file ID) is checked. A root whose own configured path is a link (for example `~/.cache -> /mnt/ssd/cache`) is resolved once, when roots are resolved, to the directory it leads to; that directory's identity is recorded then and re-checked before the root is read, and both paths are shown (D-041). Links below a root are never followed. Execution is handle-relative. A string-prefix check followed by `remove_dir_all(path)` is not acceptable.
5. **Frozen target sets.** A file plan authorizes the leaves it froze. A child that appears after review gets no authorization because its parent was selected. Directories are removed only once the approved contents are gone and they are empty, or the operation is reviewed explicitly as a whole-directory semantic operation.
6. **Revalidation can only subtract.** At apply, changed identity, activity, policy, pack digest, exclusions or required facts remove the affected targets. Revalidation never adds or substitutes a target.
7. **No privilege.** v0.1 never runs as root or asks for elevation. Privileged stores get advice only.
8. **No process control.** kipple never kills, signals or restarts anything it did not start. The only process it stops is a native command it started itself that missed its deadline (D-038).
9. **Protected subresources are host-enforced.** Each integration descriptor lists the subresources that are never candidates (for example Codex `auth.json` and `config.toml`, Claude `~/.claude.json`, settings, memory directories, skills, plugins). The host enforces this list even when the integration is absent, unsupported or fails discovery, and third-party packs can't reach inside those subresources. For a tool version that isn't supported, the whole state root is treated as protected, not just the known list.
10. **Overlap and aliasing.** Duplicate *aliases* (the same directory reached through two rules or paths) are merged into one plan entry, but only when they have the same method and scope and every applicable protection is met. The merge is never "first rule wins". Hard links stay separate unlink targets: removing one name doesn't free the inode while another survives, so reclaim estimates count unique allocations once. A parent operation (worktree removal) and a child operation (artifact deletion) on the same tree conflict and must be resolved explicitly in review. Merging never authorizes crossing a mount or a protected root.

## 4. Busy and activity detection

- A running executable's identity (exe path and file identity) protects its release, whatever a `current` link says.
- Each integration declares what activity evidence it needs: process identity, a lockfile the tool documents, or an open-handle probe where it is cheap.
- If the evidence is incomplete, the affected operation is blocked, not just the item.
- Process names alone never count as proof that something is inactive.
- Today a project artifact's activity is the process snapshot alone, scoped by D-050. Each integration declares what can show its findings in use. Cargo's `target/` needs each process's executable path. `.venv`, `__pycache__` and `node_modules` also need the working directory, the path arguments, the environment's paths and the mapped files, because `/usr/bin/python3` or `/usr/bin/node` runs them; they are also in use when a process of their runtime (`python*`; `node`, `bun`, `deno`) works in their project, or names a path there in its arguments or its environment's paths (`node ~/code/app/server.js` started from elsewhere). Any path a process shows at or inside the finding holds it as in use, compared ignoring case and by file identity. A partial inventory holds it as unknown.
- On Linux a process that sees files through another root or mount namespace is placed before it is judged (D-052). With another root in kipple's mount namespace (a chroot, such as a browser's renderer sandbox), the kernel shows its executable, working directory and mapped files as kipple names them, and the paths it named itself (arguments, environment) are compared both as written and below its root. In another mount namespace, each path it shows is resolved through `/proc/<pid>/root` and compared by the file identity of it and each directory above it; it is placed only when its executable resolves there to the file it runs, and a path that holds a link anywhere, the last component included, or a directory that can't be read leaves that kind of path unread. A root or namespace that can't be read leaves it unplaced.
- A command-line token is read whole and, when it holds whitespace (a process title rewritten into one string), word by word. A path in either reading holds a finding, but such a token is placed only when each of its words is. An option `--x=value` is read by its value. An absolute path is placed; a relative one is placed in the working directory, and can't be when that can't be read. A command line whose every token is empty or blank names nothing and counts as unread.
- Which processes count when they can't be inspected (a path the layout needs is unreadable, or on Linux the process's view can't be placed): for an artifact in the home directory, those of an unknown owner always count; the current user's count, except on Linux, where one is judged by its command line (D-052) unless the command line can't be read or names a relative path kipple can't place, the process is in another mount namespace, or its program name, or the first word of a title rewritten into one string, is unknown or a runtime (`python*`, `node`, `bun`, `deno`), which runs a project's files without naming them; root's and `SYSTEM`'s don't, nor on Linux those with root's reach: one whose effective user ID is root while its others are root's or the current user's (a setuid helper such as `fusermount3`), and another user's that holds `CAP_DAC_OVERRIDE` or `CAP_DAC_READ_SEARCH` (on Linux, any of these counts when its command line can't be read or names a relative path kipple can't place); the scan report and `doctor` count both in notices; another user's don't when a directory from home down to the artifact is private to the current user without an ACL, the artifact is on a local file system, and the process can't read past the mode bits. Outside the home directory, or when home is unknown or `/`, every one counts. On Windows in M2 kipple reads no path of a process besides its executable, so the activity of `node_modules`, `.venv` and `__pycache__`, and of any layout that needs more than the executable, is unknown there whatever runs (reason `unsupported`), and they are always Protected.
- The process probe gives every process one outcome (D-035). Only an exited process or one that is gone counts as not running. A process the OS won't let us inspect, or one with a path but no file identity, is unknown. A kernel thread runs no program and is left out like an exited process; on Linux it is recognised by the kernel-thread flag in `/proc/<pid>/stat`, which anyone can read, even though its executable is refused to a user who isn't root, and `doctor` counts it apart. A `stat` that can't be read or parsed leaves the process unknown. On Linux, a `/proc` mounted with `hidepid` or `subset` hides other users' processes without an error, so the whole inventory is partial. Both cases are incomplete evidence. Which processes count for a store is scoped by D-042: as a regular user, uninspectable root and `SYSTEM` processes don't block a per-user store, and are reported as a partial-inventory notice.

## 5. Methods and recovery

| Method | Frees space now | Undo | Used for |
| --- | --- | --- | --- |
| `delete` | yes (estimated) | no | Verified rebuildable items the user selected and reviewed (artifacts, caches, inactive releases) |
| `trash` | no, bytes stay until the trash is emptied | yes, where the backend supports listing and restoring | Opt-in for ordinary file artifacts; default for any future data-category operation |
| `native` | depends on the tool | no | The tool's own command (`npm cache clean`, `git worktree remove`...) as a reviewed semantic plan |
| `none` | — | — | Report-only findings and advice |

Rules:
- If a trash operation fails, it never falls back to permanent deletion.
- Restore refuses to overwrite, verifies identity, and reports missing or emptied trash entries.
- Native operations and whole-worktree removal are labelled "cannot be undone" in review.
- A custom quarantine store is out of scope for v0.1.

Status: the platform's trash backend is built (spike S4, D-062), and no command uses it yet (M3b). It moves an item whole with one rename that never replaces anything, from its verified parent's handle into a verified trash directory's handle on the same file system, after checking every entry against the frozen tree with the executor's checks; a changed entry, an entry left out or dropped, a link on the way, another file system or an unsupported one refuses the move and nothing moves. On Linux it is the freedesktop trash: the home trash for items on its mount, an existing `$topdir/.Trash/$uid` (`.Trash` sticky) or `$topdir/.Trash-$uid` for items on another, each owned by the user and never reached through a link, with a `.trashinfo` written first so file managers list and restore the item too. On macOS the item moves into `~/.Trash` under the name journalled before the move; kipple restores it, and Finder's Put Back doesn't. Restore moves the item back into its original directory, opened from its verified root with the identity recorded, without replacing anything; a missing or replaced directory, a link in its place, or an emptied or changed trash entry refuses. Windows has no trash method in v0.1: the Recycle Bin is reached only by path, through the shell, which deletes permanently when a volume has no bin, is set not to use it, or the item is over its quota, so `trash` is report-only there and `restore` isn't offered.

## 6. Native operations (semantic plans)

The core defines a closed enum of first-party native operations. Each variant states:
- the verified executable identity and version range
- fixed arguments with narrow typed parameters (no free-form strings)
- the scope it acts on and the exclusions it honours
- whether it can preview, and its dynamic effects (a native prune may delete things kipple did not enumerate)
- a deadline, an output bound, and a constrained environment (no user hooks; for Git, safe config isolation is verified in tests)

When a tool cannot enumerate targets up front, the review screen shows the plan as **semantic**: the command, its scope and a conservative estimate. It never shows a guessed file list. Third-party rule packs cannot reference native operations, `remove-verified-release` or `git-worktree-remove` in v1. First-party-only status comes from the capability table, not from an ID prefix.

## 7. Journal and partial outcomes

- The plan, with every item's frozen tree, is stored durably before anything runs. Then each item's intent is made durable before its first mutation, and its outcome is recorded after. One intent per item names every entry it may change, because it names the item's stored tree and what revalidation left of it; an intent per entry would add nothing but a sync per file (D-063).
- A crash between the two leaves the item `indeterminate`. Before the next mutating command runs, under the same lock, reconciliation compares a fresh freeze with the stored tree and records what it finds (unchanged, partly gone, gone, replaced) as evidence. It never counts a gone entry as removed by kipple, since something else may have removed it, and it never runs anything again. For a trash item it looks at the destination journalled with the intent, which recovers the restore handle; when the item is still at its place and that destination is empty, the move never happened, and the trash record written before it is removed. A restore a crash interrupted is reconciled the same way. Intents are matched to the stored plan by the digests stored with them, so a run an earlier version left reconciles after an upgrade.
- Every record is one line carrying its own digest. A damaged last line is what a crash leaves, and is ignored and overwritten; a damaged line followed by a sound one, or records out of order, make the run unreadable, which counts nothing in it as removed and doesn't block later runs.
- An item is `done` only when every entry the user reviewed was removed, except directories kept because new entries appeared in them or because they hold entries the plan left out; each of those is a `not-empty` note. Entries dropped at revalidation leave it `partial` (or `skipped` when none is left), each counted under its reason. An executor's report about an entry the run wasn't authorized to touch fails the item.
- A journal write that fails stops the apply: an item whose intent wasn't recorded isn't run, and nothing after it is.
- Cancellation stops scheduling new work. Items already done stay done.
- A receipt never reports success for everything when some items failed, were skipped or were cancelled.

## 8. Accounting

- `SpaceEstimate` holds apparent bytes, allocated bytes (where the platform reports them), a unique-reclaim estimate (taking hard links into account) and a completeness flag.
- Wording: "removed an estimated X of allocated storage". "Moved Y to Trash, still on disk" is reported separately.
- An observed change in free space is shown separately, labelled as observed, and not attributed to kipple when other writes may have happened.
- Network filesystems are unsupported for mutation in v0.1. They are reported, never touched.

## 8a. Privacy of receipts and diagnostics

Receipts, `doctor` output and logs store structured facts and outcomes: identities, sizes, rule IDs, reasons and error codes. They never store file contents, transcript text or credentials. Native command stderr and raw OS messages are shown live to the user in that session and kept in the in-memory error value. They are **never persisted**. Receipts store only structured fields: exit code, signal, OS error kind and kipple's error code. Paths are stored, because they are needed for restore and audit, and the docs say so. A process snapshot keeps only the path-like arguments and environment values of each process (D-035), never its full command line or environment. A title rewritten into the program name keeps only its first word and its later words with a `/` that aren't URLs. A finding in use shows only the path that matched; a path the process named itself that holds whitespace, which can carry the rest of a title, is shown as the finding itself.

## 9. Plugin authority

- Schema parsing is strict, sizes are bounded, unknown fields are rejected, and versions are checked.
- Roots are symbolic (`home.cache`, `xdg.data`, `config.project-roots`, an integration root) with bounded relative patterns. Absolute paths, `..`, alternate devices and link escapes are rejected.
- A grant is a capability tied to a resolved root identity. Packs cannot claim the whole of `$HOME`, lift protections, choose executables, grant themselves new roots, or lower a host classification.
- Packs have no network or executable access during scan or apply.
- Declarative does not mean safe. A malicious pack can still misclassify data inside its grant. That is why grants are explicit, findings show where they came from, and third-party findings stay report-only until granted.

## 10. Evidence required before a mutating release

For each OS and filesystem advertised as mutable:
- disposable integration fixtures exercising every invariant above on the real platform, not fakes
- a golden layout fixture for each supported upstream version of every mutating integration
- property tests for confinement (no path outside the root, no link escape), idempotence and deduplication
- fuzzing of manifest parsing and path normalization

Required failure scenarios:
- a tracked or custom file inside an apparent artifact
- a project that is missing or unmounted
- an owning tool that is busy, or whose state is unknown
- symlink, junction or mount escapes
- an item replaced between scan and apply
- a new child appearing after review
- non-UTF-8 paths
- hard-linked and sparse files
- pack traversal, oversized packs and scope expansion
- partial permission failure
- cancellation after the first success
- crash reconciliation
- restore conflicts
- an unsupported trash backend
- locked files on Windows

## 11. Known residual risks (documented, not hidden)

- Removed bytes are an estimate: what each removed file allocated when it had a single name. APFS clones and btrfs and XFS reflinks share storage that removing one name doesn't free, so the figure is an upper bound. Only the count of removed names is confirmed.
- A process running as the same user and actively racing kipple can still win some races. We confine and revalidate, but we don't promise atomic compare-and-delete. On Linux and macOS a file has no handle to remove it by: it is re-read without following right before `unlinkat` removes its name, so a name swapped in between is removed instead. That is a file or a link, never what the link leads to, and only inside a directory kipple checked. A directory is removed only when empty. On Windows the delete is on the handle that was checked.
- A trash move checks the item's top by name right before the rename, which moves whatever has the name by then. If a same-user process swapped it in between, the entry that arrived isn't the item: kipple moves it back and refuses. If it can't be moved back, because something has taken the name since, it stays in the trash with its `.trashinfo`, and the item fails as `stranded`, naming the slot. A restore does the same in reverse, and a stranded entry stays at the original name. A trash directory found through a top directory's path (Linux) is checked by owner, kind and identity, not by how it was reached.
- A file edited in place after the freeze, keeping its size, within one tick of the file system's timestamps (FAT keeps two seconds; Linux stamps files from a coarse clock that ticks every few milliseconds) looks unchanged and is removed. On Windows a file's times may move only once the program writing it closes it, so a same-size write through a handle still open can look unchanged too.
- A user-space file system is refused where the OS says so (FUSE on Linux and macOS). One on Windows that reports itself as NTFS is treated as NTFS.
- Upstream tools change their layouts without notice. Integrations fail closed on unfamiliar versions, which can mean less cleanup right after a tool update.
- Activity is a snapshot taken during the scan (D-050), and apply takes another, under a second old, right before each item's intent. A program that starts using an item while it is being deleted isn't seen: activity during a long delete isn't watched.
- git is asked only about repositories whose `.git` kipple finds in a directory above the item: the nearest, and every one above it, since a repository made inside a directory another already tracks files in leaves them in the outer index (D-066). A bare repository whose work tree is elsewhere, such as a dotfiles repository with `$HOME` as its work tree, leaves no `.git` there, so the files it tracks aren't seen as tracked. A file added to the index after apply asked git about its item isn't seen either. git matches paths as strings, folding at most ASCII case, while a file system may fold Unicode case and normalization, and on Windows a directory also answers to its 8.3 short name. So git is asked about the item's path only up to its first name with a byte past ASCII (on Windows, or a name other than the one the directory is stored under), and about that name's directory otherwise, and each path it returns is matched to the item on disk by identity. One spelling stays unseen: an index path whose letter past ASCII the file system folds to an ASCII one, such as the Kelvin sign for `K`, under an item whose own path is all ASCII. Both rules fail closed and can hold more than they need: a `.git` above the item that git can't use (empty, another user's, or naming another work tree) holds every item below it, and when git is asked about a directory above the item, a link or placeholder anywhere it lists holds the item.
- A root or `SYSTEM` process that kipple can't inspect isn't counted for an artifact in the home directory, and neither is one with root's reach on Linux (D-052). One running from an artifact without naming it on its command line (for example a capability-wrapped launcher started from a relative path that was rewritten) isn't seen, and neither is a root process in a container whose command line names the artifact by its path inside the container. On macOS and Windows there is no command-line check for root or `SYSTEM` processes at all: macOS denies `KERN_PROCARGS2` for other users' processes, and Windows isn't read in M2.
- On Linux a process is taken for a kernel thread, and left out, only when its `stat` carries the kernel-thread flag, which only the kernel sets. A user program can't set it, and a command name containing `)` can't fake it, because the fields are read after the last `)`.
- On Linux the current user's processes that can't be inspected are judged by their command line (D-052). One that runs a project's file without naming it there, and isn't a recognised runtime (an interpreter under another name, or a process that rewrote its title), isn't seen, and neither is one that refused its mount namespace (a container's process that isn't dumpable) and names the artifact by its path inside the container.
- A program name with whitespace keeps only the words that can name a file, so a path in it whose directory names hold a word without a `/` (`/home/u/My Big Code/…`) no longer matches whole. The words dropped hold no `/`, so none of them could be a relative path that makes the process count.
- A bind mount of a directory inside a finding, rather than of the finding or a directory above it, isn't matched by identity, in kipple's mount namespace or another. In a chroot, a link in a path the process named is followed from kipple's root by the kernel when kipple checks its identity, not from the process's.
- A command-line token is read word by word when it holds whitespace and by its value when it is an option, so a relative path whose every separator follows a space (`a /b`), or one in a directory literally named `--x=`, reads as an absolute path.
- A process can rewrite its own command line, and on Linux `/proc/<pid>/environ` is the environment it started with, so either can hide a path it uses now.
- Only path-like arguments and environment values are kept (§8a), but a secret that contains a `/`, or any word of a process title rewritten into argv[0] before its first URL, can stay in memory during a scan. It is never written or shown: a match on a token that holds whitespace shows the finding's own path.
