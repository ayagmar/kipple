# Market research

Internal document. Researched 2026-10-08 by two independent agents (Claude and Codex), with star counts from the GitHub API on that date. Feature descriptions come from each project's documentation; we did not run any competitor. Do not mention competitors by name in user-facing copy.

## Summary

- **Mole** (macOS) sets the UX bar and already cleans some AI-agent leftovers. There is no Linux build, and Windows is an experimental branch.
- **Mature general cleaners** (BleachBit) have a declarative rule format (CleanerML) and even a Claude cleaner. They are privacy-oriented, use a dated GUI, and their Claude rule wipes `~/.claude/projects` (session history) in one action.
- **Disk analyzers** (dust, dua, gdu, ncdu) are fast and polished, but they don't know what anything is.
- **Dev-artifact sweepers** (kondo across many ecosystems; npkill and cargo-sweep for one each) handle project build output only.
- **A wave of small 2026 agent cleaners**, all under ~100 stars: null-e, sweeprs, agent-gc, codex-clean, Kempt, zclean, moonbit, and a dozen Linux "Mole clones". Demand exists. Nobody has won.

The space is not empty. What is still open: a polished, cross-platform, Linux-first-class tool with an extensible rule ecosystem, honest safety semantics and agent-aware handling of typed state (versions, sessions, worktrees, models).

## Mole in detail

[tw93/Mole](https://github.com/tw93/Mole): about 69.6k stars, V1.58.0 on 2026-10-05, GPLv3. Bash (about 4.7 MB) plus Go for the `analyze` and `status` screens.

Commands: `clean`, `uninstall`, `optimize`, `analyze`, `status`, `purge` (project artifacts), `installer`, `history`, `touchid`, `completion`, `update`, `remove`.

AI-agent handling (`lib/clean/dev.sh`): keeps the active version plus N older ones of Claude Code, cursor-agent and Copilot by resolving the active symlink, cleans Codex Desktop caches and runtimes, and leaves HF/PyTorch/W&B model folders alone. `purge` knows agent worktree folders.

**Worth copying:**
- `--dry-run` everywhere
- a persistent whitelist
- an operations log (`history --json`)
- a "skipped (reason)" line for every item
- calling the tool's own cleanup command instead of `rm`
- skipping work while the owning process runs
- items touched in the last 7 days start unselected
- git-tracked files protected
- automatic JSON when piped
- vim keys and `/` search

**Weaknesses:**
- macOS only
- a huge Bash codebase
- hard-coded paths and no rule format
- `clean` and `purge` delete permanently with no undo
- the GUI is a separate paid app

## Landscape

| Tool | Stack | Stars | Activity | Platforms | Relevance / gap |
| --- | --- | ---: | --- | --- | --- |
| [BleachBit](https://github.com/bleachbit/bleachbit) | Python/GTK | 7.1k | v6.0.4, 2026-09 | Linux, Windows, early macOS | CleanerML rules, process checks, `claude.xml`. Privacy focus, no project purge, dated UX. |
| [czkawka / Krokiet](https://github.com/qarmin/czkawka) | Rust, Slint | 34k | 2026-09 | all | Duplicates and similar files. A good precedent for a reusable core with several frontends. |
| [dust](https://github.com/bootandy/dust) | Rust | 12.5k | 2026-09 | all | Shows sizes, cannot delete. |
| [dua-cli](https://github.com/Byron/dua-cli) | Rust | 6.3k | 2026-09 | all | Fast, interactive delete. No idea what things are. |
| [gdu](https://github.com/dundee/gdu) | Go | 6.1k | 2026-10 | all | Fast analyzer. |
| [ncdu](https://dev.yorhel.nl/ncdu) | Zig | n/a | 2.9.2, 2025-10 | Unix | Mature terminal standard. |
| [mcdu](https://github.com/mikalv/mcdu) | Rust | 68 | v0.6.0, 2026-08-27 | macOS, Linux | Disk browser plus dev cleanup across 18+ ecosystems, dry-run and an audit log, AUR. No Windows (per an older README, not rechecked). MIT |
| [kondo](https://github.com/tbillington/kondo) | Rust | 2.4k | 2026 | all | Project artifacts only. |
| [npkill](https://github.com/voidcosmos/npkill) | TS | 9.5k | 2026-09 | all | `node_modules` only. Shows how much a focused UX appeals. |
| [cargo-sweep](https://github.com/holmgr/cargo-sweep) | Rust | ~1k | README says unmaintained | all | Cargo only. |
| Stacer | C++/Qt | 9.3k | dead since 2024 | Linux | Optimizer GUI. |
| [null-e](https://github.com/us/null-e) | Rust | 16 | 2026-06 | all (claimed) | **Closest broad competitor**: 50+ cache types, TUI plus GUI, trash by default. |
| [sweeprs](https://github.com/salamaashoush/sweeprs) | Rust | 3 | 2026-09 | macOS, Linux | Agent sessions and worktrees, safe/caution/danger classes, dry-run by default. |
| [agent-gc](https://github.com/williamjeong2/agent-gc) | Rust | 3 | 2026-10 | 6 targets incl. Windows ARM | Agent worktrees and artifacts, risk presets. |
| [moonbit](https://github.com/Nomadcxx/moonbit) | Go | 66 | 2026-10 | Arch, Debian, Fedora, openSUSE | Closest Linux Mole-like tool. pacman/apt/dnf, Docker, systemd timer. |
| [burrow](https://github.com/caezium/burrow) | Swift + FSL engine | 1.6k | active | macOS, Windows preview | Mole-style, MCP server for agents, consent log, trash first. |
| [zclean](https://github.com/TheStack-ai/zclean) | JS | 67 | 2026-07 | macOS, Linux | Leftover agent processes (MCP servers, headless browsers). |
| codex-clean, ai-session-cleaner, Kempt, Offcut, CLV3000-Plus | various | 0–9 | 2026 | various | Narrow agent-state cleaners. ai-session-cleaner shows that sessions are linked stores, not loose files. |
| Vole, nibs, kirei, Silt, oxidclean, linux-mole, MoleLinux, winmole, squeegee | various | 0–17 | 2026 | Linux or Windows | Hobby-scale Mole ports. |

Also relevant on Arch: `paccache` (pacman-contrib) keeps three versions by default and ships a weekly timer. The ArchWiki warns that aggressive cache removal throws away the ability to roll back. Cargo has had automatic global-cache GC since 1.88. The Claude Code setting `cleanupPeriodDays` is age-based only.

## Gaps kipple targets

1. **Depth on all three OSes.** Linux package stores (pacman, apt, dnf, Flatpak, Snap, journald) need adapters, not globs.
2. **Typed agent state.** Versioned binaries (keep active + N, protect running releases), worktrees (three registration states), sessions and checkpoints (linked stores, report-only until store-specific adapters exist), model blobs (shared manifests).
3. **A rule ecosystem with real authority boundaries.** CleanerML is the established precedent. It is XML, has a wide action catalog (delete, truncate, structured edits, process checks, SQLite maintenance), and has restricted untrusted actions since 6.0. Our bet is narrower: scoped grants per root and an engine that owns execution. That is our assessment, not a verified gap. mise is moving its registry away from executable plugins for supply-chain reasons, a strong signal to stay declarative.
4. **Honest recovery and accounting.** Most tools delete permanently and report summed apparent sizes as "freed".
5. **A great Linux TUI that knows what it is looking at.**

## Plugin-system precedents

| System | Model | Trust model | Lesson |
| --- | --- | --- | --- |
| BleachBit CleanerML | Declarative XML, broad action catalog | Data only; restricts untrusted actions since 6.0 | Declarative covers most rules. |
| aqua / mise registry | Declarative YAML, pinned cosign signer | Signed and curated | mise rejects new executable plugins. Follow that. |
| Zellij | WASM | Sandbox plus a permission prompt | Good isolation, real runtime cost. |
| Extism | WASM host framework | Host functions, allowed paths | Option if we ever need computed rules. |
| Nushell | Subprocess over stdio | Version handshake, no sandbox | Typed protocol does not limit OS authority. |
| topgrade | User `[commands]` only | User-authored | Fine for personal config, not for a remote ecosystem. |

## External input: deep-research report (2026-10-09)

The founder supplied a separate deep-research report. It agreed with the core of this spec (core-owned execution, closed effect enum, unknown fails closed, revalidation at apply, native authorities, declarative packs first, a GUI over the same engine). Adopted from it, after a challenge round between Claude and Codex: Safe/Review/Protected buckets, `doctor`, profiles for unattended runs, upstream-config-first root resolution, capability-based Linux support, host-enforced protected subresources, prevention advice (`history.max_bytes`), and the GPL provenance rule for Mole. Rejected or deferred: 7–8 crates (we keep 4), a WASM tier now, session cleanup in v0.1, privileged pacman in 0.2, and preselection by default. Its citations are opaque tokens with no bibliography, so every claim taken from it was re-verified at the source on 2026-10-09 before going into these docs, except where a row says otherwise.

Verified anecdote: a Claude Code issue comment reports 13 abandoned worktrees holding 13.8 GB on one project after about 11 days. Twelve came from a different agent CLI ([#26725](https://github.com/anthropics/claude-code/issues/26725#issuecomment-5284720631)). It is a single report, not a measurement of a population.

Mole's [SECURITY_AUDIT.md](https://github.com/tw93/Mole/blob/main/SECURITY_AUDIT.md) (updated 2026-10-08) rechecks tracked files, nested repos and root identities at the deletion boundary, dedups dry-run output by device and inode, and protects AI tool runtime state. Mole is GPL-3.0. We take behaviour and ideas only, never code.

## Conclusions that shaped the spec

- Lead with explanation and safety that can be verified, not with "AI-aware". Others already claim that.
- Ship fewer adapters with verified layouts rather than a long list of glob rules.
- Declarative rule packs in v1. No downloaded executables.
- Use each tool's own cleanup command where it exists, as a reviewed semantic operation.
- Make the TUI and the landing page the reason people pick kipple over a script.
