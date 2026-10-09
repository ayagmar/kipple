# kipple — Site and Docs Plan

Status: planned. Nothing in this document is built yet.

One website serves two jobs: a landing page that makes someone want to run kipple, and docs that let them run it safely. Both live in one Astro project in `site/` and deploy together. Docs are part of the product. A page that is wrong about what kipple deletes is a safety bug.

## 1. Structure

```text
site/
  PRODUCT.md            who the site is for, what it must make them feel and do
  DESIGN.md             design system: type, color, spacing, motion (impeccable schema)
  lighthouserc.json     Lighthouse budgets, enforced in CI
  astro.config.mjs      landing routes + Starlight integration, base /kipple
  scripts/subset-fonts.sh
  src/pages/index.astro         custom landing page
  src/content/docs/**           Starlight docs (hand-written + generated)
  src/generated/**              generated inputs, never edited by hand
  tapes/*.tape                  vhs demo sources
  public/
```

Conventions copied from the user's existing sites:

| Convention | Source |
| --- | --- |
| Custom landing page, no client framework, `build.inlineStylesheets: 'always'` | `~/projects/modmgr/site/astro.config.mjs` |
| Self-hosted variable fonts via `@fontsource-variable/*`, subset by `scripts/subset-fonts.sh` | `~/projects/modmgr/site` |
| `PRODUCT.md` and `DESIGN.md` next to the site | `~/projects/modmgr/site` |
| Lighthouse: all four categories at score 1, three runs. **Changed for kipple:** median aggregation instead of modmgr's `optimistic` (see §4.5) | `~/projects/modmgr/site/lighthouserc.json` |
| Starlight for docs, `site: https://ayagmar.github.io`, `base: '/kipple'`, GitHub social link | `~/projects/niri-computer-use/site/astro.config.mjs` |
| `@astrojs/sitemap` | modmgr |

The landing page is an Astro page outside Starlight so it can have its own layout. Docs pages use Starlight with a theme that shares `DESIGN.md` tokens, so moving from the landing page into the docs feels like the same product.

## 2. Versions

Rule: newest stable release that is at least 7 days old on the decision date (2026-10-08). Final choices get recorded in `docs/decisions.md` when the site is scaffolded, and the check is redone then.

| Package | Version | Published | Notes |
| --- | --- | --- | --- |
| `astro` | 7.3.5 | 2026-09-24 | 7.3.6 (10-06), 7.3.7 (10-07) and 7.3.8 (10-08) are under 7 days old |
| `@astrojs/starlight` | 0.42.5 | 2026-10-01 | exactly 7 days old on 2026-10-08 |
| `@astrojs/sitemap` | 3.7.4 | 2026-08-31 | |
| `@lhci/cli` | 0.15.1 | 2025-06-25 | latest release; low churn, same version modmgr pins |
| vhs (charmbracelet) | v0.12.1 | 2026-09-24 | repo active (last push 2026-10-01) |

Sources: npm registry `time` metadata and GitHub releases API, queried 2026-10-08. Font packages and `sharp` are chosen when the design direction is set.

## 3. Landing page

### Who it is for

- Developers whose disks fill up and who don't know why.
- Heavy users of AI coding agents (Claude Code, Codex, Pi, Cursor and similar), whose tools leave releases, logs, worktrees and session stores behind.

Both groups come first. AI agents are the hook, and ordinary developer leftovers are most of the bytes.

### Narrative

1. **Name and hook.** "Kipple" is a word popularized by Philip K. Dick: useless stuff that piles up by itself when nobody is looking. Your tools make kipple. Say "popularized", not "coined" (earlier fandom use exists, per the SF Encyclopedia).
2. **Proof, not claims.** The hero is a real `kipple scan` output of a fixture home directory, rendered as text (selectable, readable by screen readers). It is not a screenshot and not invented numbers. Next to it: "This is a demo machine. Run `kipple scan` to see yours. It changes nothing."
3. **What it tells you.** For each finding: what it is, which tool made it, what you lose, how it comes back, and whether it is in use. This is the main difference to lead with. Disk usage alone is not the pitch.
4. **Safe by construction.** Scan is read-only. Nothing is preselected. Session history and models are shown, never removed in v0.1. Accounting is honest: "removed an estimated X of allocated storage", plus trash kept separate.
5. **Install.** One line per OS, then the package-manager channels that actually exist at that release.
6. **Extend.** Rule packs: what a third-party rule can and cannot do, in two sentences, linking to plugin trust docs.

### Copy rules

- No competitor names anywhere in user-facing copy.
- No made-up totals, testimonials, star counts, or "blazing fast". Speed claims link to the published benchmark method.
- Don't call the user's data slop. Sessions, models and worktrees are "data you may want". Kipple means leftovers that are regenerable or abandoned.
- Every command shown on the page must come from a tape or test that ran in CI.
- Run a `humanizer` pass over user-facing prose at the end of each site milestone and before release.

### Motion

Motion explains something, or it's left out. The planned piece is the hero scan streaming in group by group (Agents, Dev tools, System), the way the real TUI fills in. CSS animation and at most a small inline script. Respect `prefers-reduced-motion` with a static final frame. No scroll-jacking or parallax, and no animation that shows up as jank in a Lighthouse trace (the modmgr landing page felt laggy, so don't repeat that).

## 4. Design process

1. Write `site/PRODUCT.md`: audience, the single action (run `kipple scan`), voice, what the page must not look like.
2. Research references: the Mobbin MCP when it is available, otherwise browsing real developer-tool sites. Look at developer-tool landing pages, CLI product heroes and how terminal output is presented. Save references and reasons in `PRODUCT.md`.
3. Use the `impeccable` skill for direction, build and critique. Record tokens in `site/DESIGN.md` from the shipped page, not from intentions.
4. Review gate: the user signs off on the direction before the build. The user rejected generic, AI-looking landing pages more than once on modmgr, so get a distinct visual identity first and polish after.
5. Lighthouse 100 on all four categories, on the landing page and one representative docs page. This is a release gate under reproducible conditions: `lhci` with the mobile preset, 3 runs, median assertion, static build served locally. On PRs the job reports a regression budget (no category drops) and doesn't block merges for single-run noise.

## 5. Docs information architecture

| Section | Pages | Source |
| --- | --- | --- |
| Start | Install on Linux, Install on macOS, Install on Windows, First scan, First clean | hand-written, commands verified in CI |
| What kipple knows | Support matrix (adapter × OS × discover/report/mutate), one page per adapter with exact paths, losses, rebuild cost, native retention advice | generated from adapter metadata, plus hand-written notes per adapter |
| Safety and recovery | Safety model, what is never touched, eligibility vs category, trash and restore limits, accounting, partial failure and receipts | hand-written; must match core behavior and tests |
| Using kipple | TUI guide, `clean` flows, `--only` and automation with `--yes`, saved plans, history and restore, exclusions and config | hand-written |
| Reference | CLI reference, config file reference, exit codes, JSON result envelope, JSONL event stream, rule-pack schema | generated |
| Rule packs | Authoring a rule pack, selectors/facts/operations catalog, testing a pack with fixtures, publishing | hand-written + generated catalog |
| Plugin trust | What installing grants (nothing destructive), cleanup grants, digests and lockfile, removing a pack | hand-written |
| Contributing | Architecture (crate boundaries), adding an adapter, quality gates, release process | hand-written, mirrors `AGENTS.md` and `docs/` |
| Troubleshooting | Permission denied, busy/unknown findings, blocked items, incomplete scans, Windows locked files, network filesystems | hand-written |

Docs describe what exists in the released version. If an adapter is report-only, its page says so. Promises and roadmap items stay out of docs. The roadmap lives in the repo `docs/`.

## 6. Generated content

Anything that can drift from the binary is generated from it, and CI fails if the committed output differs from a fresh generation.

| Output | Generated from | Target |
| --- | --- | --- |
| CLI reference | clap command tree (help text, args, defaults, examples) | `site/src/content/docs/reference/cli.mdx` |
| JSON envelope and event schemas | Rust DTOs via JSON Schema derive | `site/src/generated/schema/*.json` + reference pages |
| Rule-pack schema and capability catalog | manifest types + published selector/fact/operation registry | reference + rule-pack pages |
| Support matrix | adapter descriptors (id, OS support, operations, verified tool versions) | `site/src/generated/support-matrix.json`, rendered by an Astro component |
| Hero scan and demo text | `kipple scan` against `fixtures/demo-home` | `site/src/generated/demo-scan.txt` |

Mechanism: a dev-only `xtask` binary in the workspace (`cargo xtask gen-docs`). It is not one of the four product crates and is never shipped. A hidden subcommand in the shipped binary was rejected because it would grow the user-facing surface. Precedent: `~/projects/llm-usage-metrics/scripts/generate-cli-reference.mjs` builds `site/src/content/docs/cli-reference.mdx` from the real CLI definition, and the same approach is used here, in Rust.

## 7. Demo recordings

- vhs tapes live in `site/tapes/`, one per flow: first scan, clean with review, restore, rule-pack install.
- Tapes run against `fixtures/demo-home` with `HOME` and the platform dirs pointed at a disposable copy. They never touch the CI runner's real home.
- CI renders the tapes on the Linux runner and fails if a tape errors. Rendered GIF/WebM assets are build outputs, not committed, so they always match the current binary.
- Every recording has a text transcript next to it for accessibility and search.

## 8. Hosting and deploy

- GitHub Pages at `https://ayagmar.github.io/kipple/`. A custom domain is a later decision. No domain availability has been checked.
- **The stable site is built from the release tag** with the released binary's generated reference, schemas and support matrix. Docs always describe the version people can install.
- **The `main` preview** is deployed to `/next/`, with a persistent "unreleased" banner and `noindex`. It exists so docs changes can be reviewed before release.
- **Before 0.1.0** only the preview exists. The release-candidate tag builds a candidate site, which is checked during the readiness review and promoted to the root when `v0.1.0` ships.
- The site build runs in CI on every PR once the site exists (M6): build, generated-content diff check, link check, Lighthouse budget.
- Per-version docs archives come later. Until 1.0, the root always shows the latest release.

## 9. Order of work

1. M1: `fixtures/demo-home` and `xtask gen-docs` for the CLI reference and JSON schemas (generated files are committed under `site/src/generated/` before the site exists, and `gen-docs --check` gates them).
2. M6: Astro and Starlight site, landing page, Safety and recovery pages checked against tests, tapes.
3. M7: release-candidate site, Lighthouse 100 under the release conditions, humanizer pass, user sign-off.
