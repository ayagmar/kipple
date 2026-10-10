# kipple — CI and Release Plan

Status: planned. Nothing in this document is built yet. Items marked **M-later** are out of scope for the first public release.

## 1. One gate, local and CI

The gate is `cargo xtask check`. Its authoritative step list and per-milestone activation schedule live in [08-engineering-standards.md](08-engineering-standards.md) §1. This document doesn't repeat them.

**Why `cargo xtask`, not `just` or `make`.** kipple needs native Windows contributors and runners. `just` recipes run in a shell (`sh` by default), and POSIX lines such as `RUSTDOCFLAGS=... cargo doc` don't run in PowerShell. `make` has the same problem. An `xtask` is a Rust binary in the workspace, so it runs the same everywhere with nothing extra to install, sets environment variables through `std::process::Command`, and is itself linted and tested. `.cargo/config.toml` defines `[alias] xtask = "run --locked --package xtask --"`, so building xtask can never rewrite `Cargo.lock` before the locked steps run.

Other subcommands: `cargo xtask fix` (fmt and `clippy --fix`), `cargo xtask gen-docs [--check]`, `cargo xtask site` (site build and Lighthouse, once the site exists), `cargo xtask distro <name>` (one Linux container smoke run with podman or docker).

**Local vs CI.** `cargo xtask check` on one OS is what a contributor runs. CI runs that same command on all three OSes, plus jobs that cannot run on every machine (the distro containers, the site and Lighthouse). Those extra jobs have their own `xtask` subcommands, so anyone can reproduce them locally.

Determinism: pinned toolchain in `rust-toolchain.toml`, committed `Cargo.lock`, `--locked` everywhere, gate tool versions pinned in CI. The `cargo deny` advisory database is the one input that changes over time, so a new advisory can turn a previously green commit red. That's intended. In CI, `xtask check --skip deny` runs in the matrix and `cargo deny check` runs as its own job, so the cause is obvious. Locally, the full `check` includes deny.

## 2. CI workflows

All actions are pinned to a full commit SHA with a `# vX.Y.Z` comment. Every workflow has `permissions: contents: read` by default (jobs widen only where needed) and:

```yaml
concurrency:
  group: ${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: true
```

### `ci.yml` (push to any branch, pull requests)

| Job | Runner | What |
| --- | --- | --- |
| `check` | `ubuntu-latest`, `macos-latest`, `windows-latest` | `cargo xtask check --skip deny` natively. Platform integration tests (trash backend, known-dir resolution, confinement, process probes) run here against disposable temp roots |
| `distro` | `ubuntu-latest` + containers | Builds the static musl binary once, then runs the discovery and platform smoke suite inside each distro container (see below) |
| `native` | `ubuntu-latest`, `macos-latest`, `windows-latest` | `cargo xtask test-native` (the `native` nextest profile) on the disposable runner VM, with `KIPPLE_TEST_DISPOSABLE_HOST=1`. Exists from M1 (probes) and grows in M3 (executor, trash) |
| `deny` | `ubuntu-latest` | `cargo deny check` (advisories, licenses, bans, sources) |
| `msrv` | `ubuntu-latest` | `cargo check --workspace --locked` on `rust-version` from `Cargo.toml` (**M-later** if MSRV equals the pinned toolchain) |
| `site` | `ubuntu-latest` | `cargo xtask site`: `npm ci`, `astro build`, link check, vhs tape render, Lighthouse CI (config in `site/lighthouserc.json`). Exists from M6; see the activation schedule |

Linux distro smoke matrix (containers, run on the static `x86_64-unknown-linux-musl` binary):

| Distro | Image | Why it's in the matrix |
| --- | --- | --- |
| Arch | `archlinux:base` | primary user, pacman/AUR helper discovery |
| Debian stable | `debian:stable-slim` | apt, the most common base |
| Fedora | `fedora:latest` | dnf, SELinux defaults |
| Alpine | `alpine:latest` | musl libc, busybox userland |
| openSUSE Tumbleweed | `opensuse/tumbleweed` | zypper, rolling |

Each container gets a disposable `HOME` built by `cargo xtask demo-home`. The suite runs read-only `scan --json` against fixture homes and asserts findings. Then it runs mutation fixtures against disposable roots only: trash per the freedesktop spec, receipts, confinement escapes refused. No test reads the runner's real home or uses the network. NixOS needs its own documented smoke result (**M-later**).

What CI can't prove: native mutation safety on each advertised OS is a release gate (see [04-safety-model.md](04-safety-model.md) and [11-delivery-plan.md](11-delivery-plan.md)). The matrix above is where those integration fixtures run. Unit tests with fakes don't count toward it.

### `pages.yml` (push to `main`, GitHub `release: published`; exists from M6)

There is one Pages artifact with two parts, and every run rebuilds both, so neither part can overwrite the other:
1. **Root (stable):** find the latest *published, non-prerelease* GitHub release (`gh release view --json tagName`, which returns GitHub's "latest" release), check out its tag, build the site with that release's binary, and place it at `/`. A tag whose release failed or never published (see the release flow) is never used. Release candidates are prereleases and only show up in `/next/`. Before the first release the root is a minimal page linking to `/next/`.
2. **`/next/` (preview):** build `main` with its own binary, add the "unreleased" banner and `noindex`, and place it at `/next/`.

Both outputs go into one `upload-pages-artifact`, then `deploy-pages` runs. The stable root only changes when a release finishes publishing, and every `main` push rebuilds the root from that same published release. This replaces niri-computer-use's main-only flow.

### Dependabot

Copied from `~/projects/niri-computer-use/.github/dependabot.yml`: `cargo` at `/`, `github-actions` at `/`, `npm` at `/site`, all weekly with `cooldown.default-days: 7`. The `npm` entry is added in M6, when `site/package.json` exists. The cooldown matches the "newest stable that is at least 7 days old" rule.

### Gate tool versions (decision date 2026-10-08)

Newest stable that is at least 7 days old. Re-check at scaffold time and record in `docs/decisions.md`.

| Tool | Version | Published |
| --- | --- | --- |
| cargo-nextest | 0.9.146 | 2026-09-21 (0.9.148 is from 10-08) |
| cargo-deny | 0.20.2 | 2026-07-09 |
| cargo-machete | 0.9.2 | 2026-04-15 |
| typos | 1.50.3 | 2026-09-25 (1.51.x is from 10-06) |
| `taiki-e/install-action` | v2.87.22 | 2026-09-29 |
| `actions/checkout` | v7.0.1 | 2026-07-20 |
| `actions/attest-build-provenance` | v4.2.2 | 2026-08-06 |

Source: GitHub releases API, 2026-10-08. Rechecked at M0 on 2026-10-10 with the same result; the pins in use are in [decisions.md](decisions.md) §2.

## 3. Release pipeline

### Tools

| Tool | State on 2026-10-08 | Role |
| --- | --- | --- |
| `dist` (axodotdev, formerly cargo-dist) | maintained: v0.33.0 released 2026-09-11, repo pushed 2026-10-08, not archived | builds per-target archives, checksums, shell/powershell installers, Homebrew formula, GitHub release, attestations |
| `release-plz` | maintained: v0.3.169 released 2026-09-19 (0.3.170 on 10-07), repo pushed 2026-10-08 | opens a release PR with version bumps and a changelog from Conventional Commits; tags on merge |

Pick: **release-plz for versioning and changelog, dist for building and publishing.** release-plz turns the merged release PR into a `vX.Y.Z` tag on the `kipple` package, and dist's generated `release.yml` runs on that tag.

Alternative considered: hand-written tag workflow with `gh release create`, as in `~/projects/modmgr/.github/workflows/release.yml`. That's fine for a single JS artifact. For 5+ native targets, installers, checksums and a Homebrew tap, it means rebuilding most of what dist already does.

### Workspace configuration (validated in an M7 rehearsal before enabling)

- **Lockstep versions.** All four crates share one version through release-plz's `version_group`. There is one changelog, at the root.
- **One tag.** Only the `kipple` package has `git_tag_enable = true`, with `git_tag_name = "v{{ version }}"`. The library crates have tags and GitHub releases disabled. Without this, release-plz's multi-package default produces package-prefixed tags.
- **Git-only mode.** `[workspace] git_only = true`, so release-plz takes the previous version from `v*` tags rather than from crates.io, which doesn't have these crates. `xtask` has `release = false`.
- **No crates.io publishing in v0.1.** Every crate has `publish = false` (Cargo) and `publish = false` (release-plz), so release-plz never uploads anything from `main`. The only thing that publishes is the dist workflow on the tag, after its gate. crates.io (`cargo install` and `binstall` from crates.io) is **M-later**, with its own pre-publication gate that runs *before* any upload.
- **Token.** Tags pushed with the default `GITHUB_TOKEN` don't trigger other workflows. release-plz uses a GitHub App installation token scoped to `contents` and `pull-requests` on this repo, so the tag push triggers dist. `release-plz release` only creates the tag; `git_release_enable = false`, because dist creates the GitHub release.

### Flow

1. Conventional Commits on `main` (enforced by review and `AGENTS.md`).
2. release-plz keeps an open "release vX.Y.Z" PR with `CHANGELOG.md` and version bumps.
3. Merging that PR runs `release-plz release`, which creates the `vX.Y.Z` tag and nothing else.
4. The tag triggers dist's release workflow:
   - re-runs `cargo xtask check` on all three OSes (the release gate is the CI gate),
   - builds every target,
   - generates checksums, installers and the SBOM,
   - creates GitHub artifact attestations,
   - creates the GitHub release with the changelog section,
   - runs the publish jobs (Homebrew tap, AUR).
5. If any gate fails, nothing is published. The tag stays, and the fix ships as the next patch version.

### Targets

| Target | First release |
| --- | --- |
| `x86_64-unknown-linux-musl` | yes |
| `aarch64-unknown-linux-musl` | yes |
| `x86_64-apple-darwin` | yes |
| `aarch64-apple-darwin` | yes |
| `x86_64-pc-windows-msvc` | yes |
| `aarch64-pc-windows-msvc` | **M-later**, once a native ARM Windows runner is part of the test matrix |

Linux ships static musl binaries so one artifact runs on glibc and musl distros. The published support floor (kernel, filesystems) is documented. "All distros" is not claimed.

### Supply-chain outputs

| Output | How |
| --- | --- |
| SHA-256 checksums per archive | dist `checksum = "sha256"` (default) |
| Installer checksum verification | dist's shell installer embeds archive checksums and runs `verify_checksum` before extracting (seen in `installer.sh.j2`). Confirm the PowerShell installer behaves the same before advertising it |
| Build provenance | dist `github-attestations = true` (uses `gh attestation`); users verify with `gh attestation verify` |
| SBOM | dist `cargo-cyclonedx = true` (CycloneDX per release) |
| Dependency metadata in binary | dist `cargo-auditable = true` |
| No self-updater | dist `install-updater = false`. Package-manager installs update through their manager. Installer users rerun the installer |

### Distribution channels, in order

| # | Channel | Mechanism | When |
| --- | --- | --- | --- |
| 1 | GitHub Releases + shell/PowerShell installers | dist | first release |
| 2 | Homebrew tap (repo name decided at M7) | dist `installers = ["homebrew"]`, `publish-jobs = ["homebrew"]` | first release |
| 3 | AUR `kipple-bin` (release archive) and `kipple` (source build) | custom dist publish job that renders PKGBUILDs and pushes to AUR over SSH | soon after first release |
| 4 | crates.io (`cargo install kipple`, `cargo binstall kipple`) | publish all four crates, documented as internal with no API stability before 1.0, behind a pre-publication gate | **M-later**. Reserve the names early (`kipple` was free on 2026-10-08) |
| 5 | Scoop bucket | custom job writing the manifest from release checksums | **M-later** |
| 6 | winget | manifest PR via a winget releaser action or `wingetcreate` | **M-later** |
| 7 | Nix (flake in repo, then nixpkgs) | | **M-later** |
| 8 | Distro repos (Debian, Fedora, Alpine) | packager-driven | **M-later**, not planned by us |

Package-manager installs never run our installer script and never self-update.

### Signing

| Platform | Plan |
| --- | --- |
| Windows | dist 0.33.0 supports Azure Artifact Signing and SSL.com signing for x86_64 Windows binaries and installers. Enable once a certificate/account exists (**M-later**) |
| macOS | dist's book documents Windows signing only. No macOS codesign/notarization page was found on 2026-10-08. Plan a custom post-build job using `codesign` + `notarytool` once an Apple Developer account exists (**M-later**). Until then, document the Gatekeeper behavior for downloaded binaries. Homebrew installs avoid the quarantine prompt |
| Linux | checksums + attestations; no package signing beyond what AUR/distros do |

## 4. Release readiness

Before the first public tag:

- All §1 gates green on all three OSes plus the distro matrix.
- Native mutation evidence exists for every advertised mutating adapter on every advertised OS. Adapters without it ship report-only and say so in the support matrix.
- A rehearsal on a fork ran **two successive** releases (for example `v0.0.1-rc.1`, then `v0.0.1-rc.2`). That proves release-plz's git-only version detection, the single tag, the App-token trigger, and dist producing every artifact, checksum and installer, with publish jobs disabled. The installers were run on clean VMs and containers.
- The release-candidate site is built from the candidate binary and checked in a preview deploy. The stable site is promoted from the tag (see [09-site-and-docs.md](09-site-and-docs.md) §Hosting).
- An independent review reached a READY verdict (`release-readiness-certification`).

## 5. Open items

- Confirm the PowerShell installer's checksum verification before advertising it.
- When crates.io publishing is enabled (M-later): ownership, token or trusted publishing, and the pre-publication gate. Not verified here.
- Create the GitHub App for release-plz's token.
- Homebrew tap repo name and AUR maintainer account.
