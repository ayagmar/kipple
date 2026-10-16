# kipple — Site and Docs Plan

Status: planned. Nothing in this document is built yet.

One website serves two jobs: a landing page that makes someone want to run kipple, and docs that let them run it safely. Both live in one Astro project in `site/` and deploy together. Docs are part of the product. A page that is wrong about what kipple deletes is a safety bug.

## 1. Structure

```text
PRODUCT.md              (repo root) product truth for design work, impeccable schema; exists
DESIGN.md               (repo root) design system: type, color, spacing, motion; written in M6
assets/brand/           logo, mascot, wordmark, banners, favicon; rules in BRAND.md
site/
  lighthouserc.json     Lighthouse budgets, enforced in CI
  astro.config.mjs      landing routes + Starlight integration, base /kipple
  scripts/subset-fonts.sh
  src/pages/index.astro         custom landing page
  src/content/docs/**           Starlight docs (hand-written + generated)
  src/generated/**              generated inputs, never edited by hand
  tapes/*.tape                  vhs demo sources
  tapes/*.transcript.txt        a text transcript of each recording
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
| vhs (charmbracelet) | v0.12.1 | 2026-09-24 | repo active (last push 2026-10-01); rechecked 2026-10-15, still the newest release, and in use by the `tapes` CI job (pin in `docs/decisions.md` §2) |

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

### Direction (founder, 2026-10-16)

Sleek, minimal and recognisably kipple's own. The terminal is the product, so the page is built around real output and type, not illustration. What it must not look like, because these read as generated and interchangeable:
- gradient blobs, glows, glassmorphism, purple-to-blue gradients, noise overlays
- a grid of feature cards with an icon each, emoji as bullets, "trusted by" rows
- stock 3D renders, AI-made illustrations, mock dashboards with invented numbers
- headings that announce instead of saying something ("Supercharge your workflow")

What makes it distinct comes from the brand (`assets/brand/BRAND.md`): one typeface pairing, a restrained palette taken from the TUI's bucket colours, generous space, and the streaming scan as the one moving thing. The direction is decided through 09 §4's design process and signed off before the build.

**Performance budget**, checked by `cargo xtask site` on the static build: Lighthouse 100 (09 §4); the landing page at most 150 KB compressed on first load with fonts and at most 10 KB of JavaScript; no client framework hydrated on the landing page; fonts self-hosted and subset, at most two files; no layout shift while the hero streams in.

### Motion

Motion explains something, or it's left out. The planned piece is the hero scan streaming in group by group (Agents, Dev tools, System), the way the real TUI fills in. CSS animation and at most a small inline script. Respect `prefers-reduced-motion` with a static final frame. No scroll-jacking or parallax, and no animation that shows up as jank in a Lighthouse trace (the modmgr landing page felt laggy, so don't repeat that).

## 4. Design process

1. `PRODUCT.md` at the repo root already records the audience, voice and brand commitments. Add the landing page's own strategy (the single action, `kipple scan`, and what the page must not look like) as an impeccable surface brief.
2. Research references: the Mobbin MCP when it is available, otherwise browsing real developer-tool sites. Look at developer-tool landing pages, CLI product heroes and how terminal output is presented. Save references and reasons in the surface brief.
3. Use the `impeccable` skill for direction, build and critique. Start from the brand in `assets/brand/BRAND.md`, and record `DESIGN.md` at the repo root from the shipped page, not from intentions.
4. Review gate: the user signs off on the direction before the build. The user rejected generic, AI-looking landing pages more than once on modmgr, so get a distinct visual identity first and polish after.
5. Lighthouse 100 on all four categories, on the landing page and one representative docs page. This is a release gate under reproducible conditions: `lhci` with the mobile preset, 3 runs, median assertion, static build served locally. On PRs the job reports a regression budget (no category drops) and doesn't block merges for single-run noise.

## 5. Docs information architecture

| Section | Pages | Source |
| --- | --- | --- |
| Start | Install on Linux, Install on macOS, Install on Windows, First scan, First clean | hand-written, commands verified in CI |
| What kipple knows | Support matrix (adapter × OS × discover/report/mutate), one page per adapter with exact paths, losses, rebuild cost, native retention advice | generated from adapter metadata, plus hand-written notes per adapter |
| Safety and recovery | Safety model, what is never touched, eligibility vs category, trash and restore limits, accounting, partial failure and receipts | hand-written; must match core behavior and tests |
| Using kipple | Onboarding and carefulness levels, keeping items, per-rule settings, TUI guide, `clean` flows, `--only` and automation with `--yes`, saved plans, history and restore, exclusions and config | hand-written |
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
| CLI reference | clap command tree (help text, args, defaults, examples) | `site/src/content/docs/reference/cli.mdx`; until the site exists, `site/src/generated/cli.md` (today: `scan`, `rules`, `explain` and `doctor`) |
| JSON envelope and event schemas | Rust DTOs via JSON Schema derive (`schemars`) | `site/src/generated/schema/{scan,event,rules,doctor,support-matrix}.json` today; reference pages from M6 |
| Rule-pack schema and capability catalog | manifest types + published selector/fact/operation registry | reference + rule-pack pages |
| Support matrix | adapter descriptors (id, OS support, operations, verified tool versions) | `site/src/generated/support-matrix.json` today (integration × platform × lens, roots, rule operations, verified versions); rendered by an Astro component from M6 |
| **Rule catalog: one page per built-in rule** | rule packs plus integration descriptors | `site/src/content/docs/rules/<rule-id>.mdx`, covering what it finds, why it exists, the evidence kipple checks, what you lose, how it comes back, its bucket at each carefulness level, upstream references, and how to tune or disable it (exact config snippet). A rule without `why` and `loss` text fails `gen-docs --check` |
| Hero scan and demo text | `kipple scan` against `demo-home` (from M6) | `site/src/generated/demo-scan.txt` |

Mechanism: a dev-only `xtask` binary in the workspace (`cargo xtask gen-docs`). It is not one of the four product crates and is never shipped. A hidden subcommand in the shipped binary was rejected because it would grow the user-facing surface. Precedent: `~/projects/llm-usage-metrics/scripts/generate-cli-reference.mjs` builds `site/src/content/docs/cli-reference.mdx` from the real CLI definition, and the same approach is used here, in Rust.

## 7. Demo recordings

- vhs tapes live in `site/tapes/`, one per flow: first scan and navigation (both exist), then clean with review, restore and rule-pack install. The first-scan tape runs with no config file, so it shows the onboarding; the navigation tape passes `--config` and starts on the home screen.
- Tapes run a `test-roots` build of kipple against `demo-home` (built by `cargo xtask demo-home`) through `KIPPLE_TEST_ROOTS`, which fixes the clock and empties the process table (D-046, D-047), with `KIPPLE_CONFIG` unset and `HOME` and every `XDG_*` pointed at a disposable directory, so neither kipple nor vhs and its browser touch the CI runner's real home. The default config directory is then inside the demo and empty; the job checks that before recording.
- CI renders the tapes on the Linux runner and fails if a tape errors or a screen it waits for never appears: the `tapes` job in `ci.yml` until the M6 `site` job takes them over (see [10-ci-and-release.md](10-ci-and-release.md) §2). Rendered GIF/WebM assets are build outputs, not committed, so they always match the current binary.
- Every recording has a text transcript next to it for accessibility and search (`<flow>.transcript.txt`).

## 8. Hosting and deploy

- GitHub Pages at `https://ayagmar.github.io/kipple/`. A custom domain is a later decision. No domain availability has been checked.
- **The stable site is built from the release tag** with the released binary's generated reference, schemas and support matrix. Docs always describe the version people can install.
- **The `main` preview** is deployed to `/next/`, with a persistent "unreleased" banner and `noindex`. It exists so docs changes can be reviewed before release.
- **Before 0.1.0** only the preview exists. The release-candidate tag builds a candidate site, which is checked during the readiness review and promoted to the root when `v0.1.0` ships.
- The site build runs in CI on every PR once the site exists (M6): build, generated-content diff check, link check, Lighthouse budget.
- Per-version docs archives come later. Until 1.0, the root always shows the latest release.

## 9. Order of work

1. M0: `xtask gen-docs` for the CLI reference (`--help` and `--version`). M1: `cargo xtask demo-home`, `cargo xtask verify-demo`, and `gen-docs` for the JSON schemas and the support matrix (generated files are committed under `site/src/generated/` before the site exists, and `gen-docs --check` gates them).
2. M6: Astro and Starlight site, landing page, Safety and recovery pages checked against tests, tapes.
3. M7: release-candidate site, Lighthouse 100 under the release conditions, humanizer pass, user sign-off.
