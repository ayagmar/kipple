# AGENTS.md

1. Run `cargo xtask check` before declaring any change done. A red gate means the change is not done.
2. Never add `#[allow]`. Use `#[expect(lint, reason = "…")]` only when the lint is wrong for that one item.
3. Do not loosen a lint, threshold, `clippy.toml` disallowed method or `deny.toml` rule to make a change pass. Changing a gate is its own change with a stated reason.
4. Respect the crate boundaries in `docs/05-architecture.md`. `kipple-core` depends on no other kipple crate and has no I/O. `kipple-adapters` and `kipple-platform` never depend on each other. `#[cfg(target_os)]` lives only in `kipple-platform`. Only the binary composes them.
5. Integrations and rules propose. The core decides. Only the platform executor mutates the filesystem or runs native commands. Never call `std::fs::remove_*`, `rename` or `Command::new` anywhere else. The one exception is the dev-only `xtask`, which launches cargo and the gate tools from `xtask/src/cmd.rs`.
6. Unknown means blocked. Never default a missing fact, unfamiliar layout or failed process probe to "safe" or "idle". Read `docs/04-safety-model.md` before touching policy, plans, the executor or an integration.
7. A new integration or upstream version needs a layout fixture in `crates/kipple-adapters/tests/fixtures/` built with `FixtureTree`. Never write a rule from guessed paths.
8. Tests never touch the real `$HOME`, real tool stores, the real trash, the external network, or the user's git config and hooks. The only network test is the marked loopback HTTPS pack integration test. Inject roots and the clock. Native-backend tests run only in the `native` nextest profile (see `docs/08-engineering-standards.md` §5).
9. Tests encode domain behaviour and safety invariants, and are derived from the spec (`docs/04-safety-model.md` §10, the integration fixtures in `docs/06-adapters-and-rules.md`, and the verify lines in `docs/11-delivery-plan.md`), never from the code just written. Name the bug a test catches before writing it. Don't test getters, derives, framework behaviour or mocks that return what they were told. There is no coverage target.
10. Keep functions small and flat within the clippy limits. Split by responsibility, not to dodge a number.
11. Prefer types over comments: newtypes for IDs, byte kinds and roots. Internal items are `pub(crate)`.
12. stdout is data, stderr is diagnostics. Final reports are sorted deterministically. The event stream follows emission order. JSON schemas are versioned and generated from the types.
13. Do not add a dependency without an entry in `docs/decisions.md` giving why, the version and its publish date. Use the newest stable release at least 7 days old.
14. Conventional Commits: `type(scope): summary`, imperative, lowercase, at most 72 characters. No AI attribution, `Co-authored-by` lines or emoji.
15. Docs describe what exists now and change in the same commit as the behaviour. User-facing copy never names competitors and never calls user data "slop".
16. Write everything independently. Never copy code, tests or substantial docs from projects with incompatible licences (Mole is GPL-3.0; FrankenTUI carries an anti-AI rider). Behaviour and ideas are fine.
