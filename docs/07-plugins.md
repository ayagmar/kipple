# Rule packs (remote plugins)

Status: planned.

## 1. Decision

Third-party extension in v1 is **declarative rule packs only**. They contain no downloaded executables, subprocess plugins or WASM. Being installable remotely doesn't require running remote code.

- **Subprocess plugins are rejected.** A typed protocol doesn't limit OS authority. A child process inherits the user's rights, and portable sandboxing across three OSes is not realistic.
- **WASM is deferred.** We revisit it only when a real rule demonstrably can't be expressed over published selectors and facts. The direction recorded if it is ever triggered (an option, not a commitment): WebAssembly Component Model with a WIT world that exports `probe` and `discover` and returns typed candidates only. No WASI preopens or network (Wasmtime's `WasiCtxBuilder` defaults to none). The plugin gets only imported host functions that return scoped handles (inventory of a granted root, stat, git worktree list), plus bounded fuel, memory, time and output, and every piece of evidence it returns is validated as untrusted. Host imports can leak authority too, so each one goes through the same review as a new capability. Deletion stays in core either way.

## 2. Pack format

A pack is a single TOML document in v1, which avoids archive traversal issues. The normative schema is [06-adapters-and-rules.md](06-adapters-and-rules.md) §3. The host requires `activity = "idle"` for every mutating operation, so packs don't declare it. Multi-file packs can come later.

```toml
schema_version = 1

[pack]
id = "dev.example.bun"                # reverse-domain, unique
version = "1.2.0"                     # immutable once published
publisher = "example.dev"
description = "Bun install cache and build artifacts."
license = "MIT"
source = "https://github.com/example/kipple-bun"
kipple = ">=0.1, <0.3"                 # engine compatibility
platforms = ["linux", "macos", "windows"]

[[rule]]
id = "dev.example.bun.install-cache"
integration = "generic"
title = "Bun install cache"
category = "cache"
loss = "Packages re-download on next install."
[rule.scope]
root = "home"
selection = "dirs-matching"
pattern = ".bun/install/cache"
[rule.operation]
kind = "remove-files"
methods = ["delete", "trash"]
```

Validation is strict and follows the limits in 06 §3. Links are never followed.

## 3. Install, grant, update

```text
kipple pack add https://example.dev/kipple-bun-1.2.0.toml --sha256 <digest>
kipple pack add gh:example/kipple-bun@v1.2.0     # later: fetches tagged file, pins commit
kipple pack grant dev.example.bun [rule] [--root home]
kipple pack update [pack]                         # shows rule/root diff, re-asks grants on expansion
kipple pack remove dev.example.bun                # removes pack + grants; never files it once matched
```

- **Fetch** is HTTPS only, with limits on bytes, time and redirects. Nothing runs through a shell or git hooks. There is no network access during scan or apply.
- **Install** stores the exact manifest bytes, the digest and the source in `packs.lock`, with no grants.
- **Before a grant**, findings appear with a `needs grant` label and are report-only.
- **Grants** cover a rule (or a whole pack) on a resolved root identity. They show exactly which paths the rule could reach on this machine.
- **Updates** never happen silently. Any expansion of roots, selectors or operations needs a new grant.
- **Provenance** is shown on every finding: pack, version, publisher, digest.

## 4. Registry (later)

- **v0.2:** a curated index repo (`kipple-dev/packs`) listing pack ID, source, versions and digests. Packs that pass review get a `reviewed` badge. "First-party" is a trust status in the index, not a name prefix.
- **v0.3:** signatures with pinned publisher keys (minisign or Sigstore, decided by spike), plus revocation through the index. Signatures prove who published a pack, not that it is safe.

Until then the docs say plainly that an out-of-band `--sha256` makes installs reproducible but doesn't authenticate the publisher.

## 5. Authoring experience

- `kipple pack new` scaffolds a pack with one rule and a fixture.
- `kipple pack check FILE` validates the schema, lints patterns and runs the pack against its fixtures. Authors get errors with line and column.
- `kipple pack try FILE` scans with the pack loaded temporarily, read-only and with no grants.
- The docs site has an authoring guide, the published capability set for each kipple version, and worked examples.
