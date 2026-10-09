# Safety model

Status: planned. These are release gates. An operation that cannot meet them on a given OS or filesystem stays report-only there, with the limitation published. A failing integration doesn't block releasing the integrations that pass.

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
| 2 | **Safe** | eligible, *and* `recovery` is `automatic` or `rebuild-local`, *and* `loss` is a subset of {`cache-warmth`}, *and* `cost` isn't `high`, *and* not recent, *and* the method is available and granted, *and* there is no conflict with another operation on the same tree |
| 3 | **Review** | every other eligible finding: `reinstall`, `redownload` or `none` recovery, any other loss, high cost, recent, or in conflict |

An unknown `cost` is optional. It doesn't block Safe, and the detail pane shows "restore cost unknown". Any combination that includes a re-download (for example "rebuild needs a re-download") is `reinstall` or `redownload`, and so lands in Review.

- **Consequence is composable.** `loss` is a set, and `recovery` and `cost` are separate facts. A package cache prune loses rollback and offline installs at once.
- **Consequence facts come from the host.** For typed integrations they come from verified semantics. For third-party `remove-files` rules, recovery is `none`, so by row 3 they always land in Review (or Protected) and can never be Safe. A pack's `loss` prose is display text, never input to classification.
- **Unknown is never Review.** Missing activity or required facts block mutation. More confirmation can't make up for missing evidence.
- **Optional facts don't gate.** An unknown optional fact (for example the restore cost estimate) shows as "unknown cost". It doesn't make the item Safe, and it doesn't block it.
- **Native doesn't mean disposable.** A tool's own command constrains *how* something is removed, not *what is lost*. Package cache pruning and journal vacuuming lose rollback and diagnostics, so they are Review.
- **"Safe" means passing the current checks**, not "risk-free". The detail pane lists residual risks (§11).

## 3. Invariants

1. **Scan is read-only** with respect to discovered data: no VACUUM, no lockfiles, no native maintenance, no network. kipple writes only its own state.
2. **Protected sets:** filesystem roots, `$HOME` itself, config and credential stores, VCS sources (tracked files), and tool data whose semantics are not modelled. Protection comes from ownership semantics, not from a filename blocklist alone.
   - **One narrow exception:** the first-party `git-worktree-remove` operation may remove a *linked* worktree's checkout (tracked files included) through `git worktree remove` without `--force`. All of the facts in [06-adapters-and-rules.md](06-adapters-and-rules.md) §4 must be satisfied. The operation is bound to the verified repository and registration identity, it gets its own review line marked "cannot be undone", and it is never available to third-party packs. Tracked-file protection holds everywhere else, including inside artifact directories.
3. **Unknown means blocked.** This covers an unsupported layout or schema version, unknown ownership, an unreadable subtree, an uncertain busy state, and a changed fingerprint, policy or pack digest. If process observation fails, that is never read as "idle".
4. **Confinement.** Operations stay inside an approved root capability. Traversal never follows symlinks or junctions. Root identity (device and inode, or the Windows file ID) is checked. Execution is handle-relative. A string-prefix check followed by `remove_dir_all(path)` is not acceptable.
5. **Frozen target sets.** A file plan authorizes the leaves it froze. A child that appears after review gets no authorization because its parent was selected. Directories are removed only once the approved contents are gone and they are empty, or the operation is reviewed explicitly as a whole-directory semantic operation.
6. **Revalidation can only subtract.** At apply, changed identity, activity, policy, pack digest, exclusions or required facts remove the affected targets. Revalidation never adds or substitutes a target.
7. **No privilege.** v0.1 never runs as root or asks for elevation. Privileged stores get advice only.
8. **No process control.** kipple never kills, signals or restarts anything.
9. **Protected subresources are host-enforced.** Each integration descriptor lists the subresources that are never candidates (for example Codex `auth.json` and `config.toml`, Claude `~/.claude.json`, settings, memory directories, skills, plugins). The host enforces this list even when the integration is absent, unsupported or fails discovery, and third-party packs can't reach inside those subresources. For a tool version that isn't supported, the whole state root is treated as protected, not just the known list.
10. **Overlap and aliasing.** Duplicate *aliases* (the same directory reached through two rules or paths) are merged into one plan entry, but only when they have the same method and scope and every applicable protection is met. The merge is never "first rule wins". Hard links stay separate unlink targets: removing one name doesn't free the inode while another survives, so reclaim estimates count unique allocations once. A parent operation (worktree removal) and a child operation (artifact deletion) on the same tree conflict and must be resolved explicitly in review. Merging never authorizes crossing a mount or a protected root.

## 4. Busy and activity detection

- A running executable's identity (exe path and file identity) protects its release, whatever a `current` link says.
- Each integration declares what activity evidence it needs: process identity, a lockfile the tool documents, or an open-handle probe where it is cheap.
- If the evidence is incomplete, the affected operation is blocked, not just the item.
- Process names alone never count as proof that something is inactive.

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

## 6. Native operations (semantic plans)

The core defines a closed enum of first-party native operations. Each variant states:
- the verified executable identity and version range
- fixed arguments with narrow typed parameters (no free-form strings)
- the scope it acts on and the exclusions it honours
- whether it can preview, and its dynamic effects (a native prune may delete things kipple did not enumerate)
- a deadline, an output bound, and a constrained environment (no user hooks; for Git, safe config isolation is verified in tests)

When a tool cannot enumerate targets up front, the review screen shows the plan as **semantic**: the command, its scope and a conservative estimate. It never shows a guessed file list. Third-party rule packs cannot reference native operations, `remove-verified-release` or `git-worktree-remove` in v1. First-party-only status comes from the capability table, not from an ID prefix.

## 7. Journal and partial outcomes

- The intent is journalled before each mutation, and the outcome after.
- A crash between the two leaves an `indeterminate` entry, which is reconciled on the next start.
- Cancellation stops scheduling new work. Items already done stay done.
- A receipt never reports success for everything when some items failed, were skipped or were cancelled.

## 8. Accounting

- `SpaceEstimate` holds apparent bytes, allocated bytes (where the platform reports them), a unique-reclaim estimate (taking hard links into account) and a completeness flag.
- Wording: "removed an estimated X of allocated storage". "Moved Y to Trash, still on disk" is reported separately.
- An observed change in free space is shown separately, labelled as observed, and not attributed to kipple when other writes may have happened.
- Network filesystems are unsupported for mutation in v0.1. They are reported, never touched.

## 8a. Privacy of receipts and diagnostics

Receipts, `doctor` output and logs store structured facts and outcomes: identities, sizes, rule IDs, reasons and error codes. They never store file contents, transcript text or credentials. Native command stderr and raw OS messages are shown live to the user in that session and kept in the in-memory error value. They are **never persisted**. Receipts store only structured fields: exit code, signal, OS error kind and kipple's error code. Paths are stored, because they are needed for restore and audit, and the docs say so.

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

- A process running as the same user and actively racing kipple can still win some races. We confine and revalidate, but we don't promise atomic compare-and-delete.
- Upstream tools change their layouts without notice. Integrations fail closed on unfamiliar versions, which can mean less cleanup right after a tool update.
