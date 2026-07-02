## What and why

<!-- One paragraph: the problem, the approach, anything a reviewer should look at first. -->

## Checklist

- [ ] `CHANGELOG.md` entry under `[Unreleased]`
- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and the test suite pass locally
- [ ] Docs updated if behavior or API changed (`README.md` / `DEVELOPER.md` / `SPEC.md` / rustdoc)
- [ ] Feature subsets still build if per-VM enums or `#[cfg(feature = ...)]` sites changed
- [ ] No network, env vars, or funded wallets required by the default `cargo test`
