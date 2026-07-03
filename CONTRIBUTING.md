# Contributing

Thanks for considering a contribution. This page covers the mechanics; `DEVELOPER.md` covers the architecture and `docs/adding-a-vm.md` the full checklist for a new chain ecosystem.

## Setup

* Rust stable (the MSRV is `rust-version` in the workspace `Cargo.toml`).
* The pure unit tests in each crate need nothing else: `cargo test -p cross-vm-core` (or any VM crate) works on a fresh checkout.
* The integration tests embed contract artifacts at compile time, so they need the per-ecosystem toolchains once: Foundry (`forge`) for EVM, the Anza platform tools (`cargo-build-sbf`) for Solana, and Node + pnpm + tronbox for Tron. Run `make compile` after installing them (see the Makefile's `setup-*` targets). Until then, `cargo test -p cross-vm-tests` will not compile; everything else will.

## Before opening a PR

Run what CI runs:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude cross-vm-cosmwasm
cargo test -p cross-vm-cosmwasm --lib --test counter --test vault --test typed_handle
cargo doc --workspace --no-deps
```

Plus, when your change touches dependency or feature surfaces:

```
cargo deny check
cargo hack check -p cross-vm-framework --feature-powerset --depth 2 --at-least-one-of cw,evm,solana,tron --no-dev-deps
```

And add a `CHANGELOG.md` entry under `[Unreleased]` (Keep a Changelog format; say why, not just what).

## Ground rules

* **The default suite is mock-first.** `cargo test` must pass with no network, no env vars, and no funded wallets. Live-RPC behavior belongs in `#[ignore]`d tests (`tests/rpc.rs` / `tests/onchain.rs`) or behind opt-in cargo features, and secrets only ever come from the local `.env` (see `.env.example`); never commit one.
* **Feature subsets must build.** Every VM is a cargo feature on `cross-vm-framework`. If you add a per-VM enum variant or match arm, gate it and check a subset build (the `features` CI job will catch it otherwise).
* **Property-test failures must stay reproducible.** Harness tests print their seed; a bug report or regression test should carry the seed and mode, and ideally the minimized history from `shrink`. Do not commit tests whose failure cannot be replayed from a seed.
* **Pinned majors are deliberate.** `revm`, `cw-multi-test`/`cosmwasm-std`, `litesvm`/`solana-*`, and `alloy` majors are pinned with rationale in `DEVELOPER.md`. Bumping one is its own PR with the rationale updated.

## Reporting bugs

Use the bug issue template. For harness (fuzz/invariant/endurance) failures, include the printed seed, the run mode, and the failing invariant or bug detail; with those, the failure replays deterministically.
