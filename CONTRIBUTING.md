# Contributing to this Herdr fork

`obrr-hhx/herdr` is an independent community fork of [Herdr](https://github.com/herdrdev/herdr).
**Pull requests are welcome from everyone. There is no contributor allowlist.**
These rules apply to this fork; upstream has its own contribution policy.

## Report a problem or propose a change

Search existing issues first. Include observed and expected behavior, reproduction steps
when available, your environment, and a small sanitized log excerpt. Intermittent reports
are welcome: say what is known and what has not been reproduced.

Small bug fixes can be submitted directly. Discuss new features, architecture changes,
or substantial UI behavior changes in an issue or Discussion before implementing them.
A report does not reserve the work; link related discussions so others can coordinate.

## Submit a pull request

1. Fork this repository and create a branch from `master`.
2. Read `AGENTS.md`. Keep one PR focused on one problem and preserve existing behavior
   outside that scope. Keep the original license and copyright notices.
3. Add a regression test that fails without a bug fix. Explain the reproduction,
   resulting behavior, relevant edge cases, and verification in the PR description.
4. Install the repository hooks with `just install-hooks`.
5. Use `rust-toolchain.toml`, Zig 0.15.2, Bun 1.3.14, `just`, and `cargo-nextest`.
   On macOS use Homebrew's patched `zig@0.15` and put its `bin` directory on PATH.
6. Run `just ci` before opening or updating a PR. On macOS run
   `just ci 'not binary(live_handoff)'`, matching the CI matrix. Report any blocker;
   do not describe incomplete checks as passing. CI also checks Linux and Windows.
7. Open the PR against **obrr-hhx/herdr**, with a lowercase conventional title such
   as `fix: recover input after a failed machine switch`.

Use `refs #N` in commit bodies for this fork's issues, and full URLs for upstream
issues. Avoid `fixes`, `closes`, or `resolves` when a change has not been released.
Update unreleased documentation under `docs/next/` when behavior changes. Do not
rewrite historical upstream release notes or published documentation.

Human-written and AI-assisted contributions follow the same requirements. You should
understand the change and be able to explain what it does and what its tests prove.
There are no required attribution footers. Maintainers review changes for correctness,
scope, compatibility, and maintainability; submitting a PR does not guarantee acceptance.

## Maintainer

The maintainer of this fork is [@obrr-hhx](https://github.com/obrr-hhx).
