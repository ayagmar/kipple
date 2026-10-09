# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

The design surfaces are the website (landing page and docs) and the terminal UI. The product itself is a CLI/TUI for Linux, macOS and Windows. There is no GUI before CLI/TUI 1.0.

## Stack

Website: Astro with a custom landing page plus Starlight for docs, deployed to GitHub Pages (confirmed 2026-10-09; see `docs/09-site-and-docs.md`). Terminal UI: Rust with ratatui.

## Users

- **Lead audience: developers who use coding agents heavily** (Claude Code, Codex, Pi, Cursor and others). Their disks fill fastest with agent releases, worktrees, sessions, logs and build output, and they understand the least about what the agents keep.
- **Polyglot developers** whose Rust, Node, Python, JVM and container tooling each keep their own caches and build output.
- **Everyone else must not be left behind.** People who don't know what `target/` or `$XDG_STATE_HOME` means get the plain three-bucket view (Safe to clean, Review, Protected), and the depth stays one keypress away.

## Product Purpose

kipple finds what your tools leave behind, explains in plain words what each thing is, why it's there and what you lose by removing it, and removes what is safe to remove, keeping a record of everything it did. Success means reclaiming real gigabytes on a typical machine with no surprises, and never deleting anything the user didn't select and review.

## Positioning

kipple must always be able to explain why it believes something is safe to remove. Integrations and rules only propose. One engine decides and executes, re-checks everything at the moment of deletion, and treats anything uncertain as blocked. It knows the typed state of coding agents (versioned releases, worktrees from any agent, linked session stores), not just folder names. Anyone can extend it with declarative rule packs that need a user grant before they can touch anything.

## Operating Context

- Run from a terminal, typically when a disk is filling up or as an occasional tidy-up. `kipple` with no arguments opens the TUI. Scripts and automation use `--json`, `--events jsonl` and saved profiles.
- First run includes onboarding: a carefulness level (Careful, Balanced, Thorough) with live numbers for the machine, project roots, and the default method (delete or Trash).
- Every deletion goes through a review screen and an explicit confirmation.
- The website is where people decide to try kipple and where they check exactly what a rule does before trusting it. There is one generated docs page per rule.

## Capabilities and Constraints

- Specification: `docs/01`–`11` and `docs/decisions.md`. Status: specification only. No code is implemented yet.
- Vocabulary is fixed in `docs/03-functional-spec.md` §1: finding, rule, pack, category, eligibility, method, selection, plan, receipt, grant, advice, consequence, bucket, profile.
- Never in scope: system "optimisation", killing processes, running as root (until a separate design), secure wipe, telemetry.
- Undecided: custom domain, Homebrew tap name.

## Brand Commitments

- **Name:** `kipple`, always lowercase. It's a word popularized by Philip K. Dick for useless stuff that piles up by itself when nobody is looking. Say "popularized", never "coined".
- **Mascot: Kip, a cute flat-style sea otter.** Kip keeps one glowing amber pebble (the thing worth keeping) and tosses away the clutter. This mirrors "keep what matters" (`p` pins an item). The approved logo was redrawn as SVG in `assets/brand/` (see `BRAND.md`). The approved full-body pose sheet, `assets/brand/source/mascot-sheet.jpg`, is the reference for drawing the poses in M6.
- **Logo:** Kip's head, front view. Two variants: the head with paws holding the pebble (larger uses), and the head alone (16 px and terminal uses). Light and dark versions.
- **Colours set by the approved assets:**

  | Role | Hex |
  | --- | --- |
  | ink | `#15171C` |
  | bone/cream | `#F4EADE` |
  | otter fur | `#CA8848` |
  | amber pebble | `#F2A23A` |
  | dark-mode outline brown | `#4A2A1C` |

  Amber is reserved for "what matters / what's kept".
- **Voice: calm, precise, warm.** Wording about what gets deleted and why is plain and exact. Kip adds a little warmth in onboarding and empty states, and never jokes about the user's data. The product UI never calls user data "slop".
- **Copy rules:** no competitor names in user-facing copy, no invented numbers, testimonials or "blazing fast" claims, and every command shown must come from a run that worked.

## Evidence on Hand

- One-off founder measurements from 2026-10-08 (`docs/01-product-vision.md` §1), labelled as anecdotal.
- One public report: 13 abandoned agent worktrees holding 13.8 GB on one project, 12 of them from a non-Claude agent (`docs/02-market-research.md`).
- Missing, so it must not be fabricated: users, testimonials, benchmarks, download counts, stars, press.

## Product Principles

1. Explain before acting. If kipple can't explain it, it doesn't offer it.
2. Unknown means blocked, and careful defaults that clean nothing are a failure too.
3. Nothing is selected for the user, and nothing is deleted without review and confirmation.
4. Prevention beats repeated deletion: surface each tool's own retention setting first.
5. Small, verified coverage beats a long list, and every rule is configurable and documented.

## Accessibility & Inclusion

- Terminal: works at 80×24, honours `NO_COLOR`, has an `--ascii` mode, never uses colour alone to carry meaning, and escapes control characters in paths and plugin text.
- Website: Lighthouse 100 in all four categories under the conditions set in `docs/09-site-and-docs.md`, and every recording has a text transcript.
