# kipple: product vision

Status: planned. Nothing here is implemented yet.

## 1. Problem

Developer machines collect storage nobody chose to keep. Coding agents, toolchains and package managers write caches, old releases, logs, worktrees and build output, and none of them clean up after each other. Most people never find out until the disk is full, and then they get no answer to the questions that matter: what is this, who made it, what do I lose if it goes, and how do I get it back?

The scale is not hypothetical. Founder's observations on their Arch workstation, 2026-10-08 (`du -sh --apparent-size`, a one-off measurement, not a benchmark):

| Location | Size | What it is |
| --- | ---: | --- |
| `/var/cache/pacman/pkg` | 7.4 GB | 3,553 cached packages |
| `~/projects/*/target` | 10.0 GB | Rust build output in two projects |
| `~/.npm` | 4.8 GB | npm download cache |
| `~/.codex/packages` | 3.1 GB | 8 Codex releases across two components. `current` links mark the active ones. An older release was still running at the time |
| `~/.cache/yay` | 2.5 GB | AUR helper build cache |
| `glowly/.claude/worktrees` | 1.4 GB | 8 agent worktrees left behind, with `node_modules` |
| `~/.cache/codex-runtimes` | 1.7 GB | Codex runtime downloads |
| `~/.local/share/Trash` | 1.1 GB | Already-deleted files, still on disk |

During the same session, Codex updated itself and added another 430 MB release while the previous release was still running in another pane. The tool has to cope with that kind of thing: data that changes under you, versions that are in use, and stores owned by other programs.

## 2. Vision

kipple finds what your tools leave behind, explains it in plain words, and removes what is safe to remove, with a record of everything it did.

The rule above all others: **kipple must always be able to explain why it believes something is safe to remove.** If it can't, the item isn't offered. The internal noun is *finding*, not file. The verb is *apply*, not delete. The output is *evidence*, not a glob match.

It runs the same way on Linux, macOS and Windows, knows the footprint of coding agents and developer toolchains, and anyone can extend it with rule packs that the engine still checks before anything is touched.

## 3. Who it is for

1. **People who use coding agents heavily** (Claude Code, Codex, Pi, Cursor and others). Their machines grow fastest and they understand the least about what the agents store.
2. **Polyglot developers.** Rust, Node, Python, JVM and container tooling each keep their own caches and build output.
3. **Everyone else.** Browsers, IDEs, package managers, old downloads and the trash all add up. Normal users see three plain buckets (Safe to clean, Review, Protected) and never need to learn what `target/` or `$XDG_STATE_HOME` means. Developers get the evidence, `explain`, `doctor` and JSON underneath. One product, with progressive disclosure.

## 4. Positioning

"Mole, but cross-platform and in Rust" does not hold up on its own. Mole already cleans a handful of agents on macOS, and smaller Rust tools (null-e, sweeprs, agent-gc) claim parts of this space. kipple's edge has to be things a user can check:

- **Every finding explains itself.** It says what the item is, which tool owns it, what you lose, how it comes back, and why something was blocked.
- **The core decides and executes. Adapters never delete.** Built-in integrations and third-party rule packs only propose. The engine checks every operation against the same invariants, whichever frontend asked for it.
- **Honest accounting.** "Removed an estimated 9.2 GB of allocated storage" is kept separate from "moved 1.1 GB to Trash, still on disk".
- **Linux, macOS and Windows are all first-class**, with a published support matrix instead of "works everywhere".
- **An open rule format** with scoped grants, so the community can add coverage without being able to delete anything outside what the user granted.

## 5. Principles

1. **Explain before acting.** Every candidate shows its reason, its owner, the loss and the way back. If we cannot explain it, we do not offer it.
2. **Unknown means blocked.** Missing facts, unfamiliar layouts, uncertain process state or changed identity mean report-only, with the reason shown.
3. **Nothing is selected for you.** Selection is always the user's act. Groups are for navigation, not permission.
4. **Prevention beats repeated deletion.** When a tool has its own retention setting (Claude's `cleanupPeriodDays`, Codex's `history.max_bytes`, `paccache.timer`), kipple shows it and explains the recurring growth before offering to delete anything.
5. **Fast without being greedy.** Known roots appear in under 250 ms. The TUI stays responsive while scanning. Work is bounded and can be cancelled.
6. **Local and private.** No telemetry and no network during scan or apply. Plugin installs are the only network operation, and the user starts them.
7. **One engine, many frontends.** CLI, TUI, JSON consumers and a future GUI all drive the same core. No frontend has its own cleanup logic.
8. **Small, verified coverage beats a long list.** An adapter ships for a tool only once its layout and lifecycle have fixtures. Supporting a tool's name is not the same as supporting its cleanup.

## 6. Non-goals

- System "optimisation": RAM purges, DNS flushes, service restarts, registry cleaning, killing processes.
- Running as root or escalating privileges in v0.1. System-owned stores get a report and a copyable native command.
- Uninstalling applications (possibly later, per platform).
- Secure wipe.
- Calling user data "slop" in the product UI. Sessions, models and worktrees can be valuable; marketing can be playful, the UI stays precise.
- Promising that cleaning makes a machine faster. Clearing caches often slows the next build or forces a download, and kipple says so.

## 7. Success criteria

1. On a typical agent-heavy dev box, `kipple` shows a correct, explained overview in under one second and reclaims multiple gigabytes with no surprises.
2. No reported case of kipple deleting data outside an item the user selected and reviewed.
3. A third party can publish a rule pack that users install, review and grant without any change to kipple itself.
4. The GUI milestone ships without changing the core's public contracts in a breaking way.
5. Docs and landing page reach Lighthouse 100 in all four categories, and people describe the site as distinctive, not templated.

## 8. Name

**kipple**: a word popularized by Philip K. Dick in *Do Androids Dream of Electric Sheep?* for useless objects that pile up on their own when nobody is looking: "Kipple drives out nonkipple." It is a precise description of what coding agents and toolchains do to a disk, and it gives the landing page its story.

Collision checks (2026-10-08): crates.io free, Homebrew formula and cask free, no AUR or Repology packages, no GitHub project in this category. An abandoned npm package named `kipple` (v0.0.2, 8 downloads a month, last push 2023) installs a `kipple` binary. We accept that risk; see [decisions.md](decisions.md) D-001. Runner-up: **shedkit**.
