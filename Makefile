.PHONY: compile compile-solidity compile-solana compile-cosmwasm compile-tron setup-solidity setup-tron fmt \
	test test-cosmwasm test-solidity test-solana test-examples test-harness test-cross-vm \
	test-fuzz test-invariant test-endurance test-rpc-endurance test-harness-all \
	test-examples-evm test-examples-cosmos test-examples-solana test-examples-tvm test-examples-all

# Contract artifacts are embedded at compile time, so `make compile` (or the matching per-VM
# `compile-*` target for the mocks feature you enable) must run before any cargo build that pulls a
# cross-vm-common VM feature. `cargo test --workspace` unifies all VM features and needs `make compile`;
# `cargo test -p evm-tests` needs only `make compile-solidity`, `-p tvm-tests` only `make compile-tron`,
# `-p solana-tests` only `make compile-solana`; `-p cosmos-tests` needs no compiled artifact.

compile: compile-solidity compile-solana compile-cosmwasm compile-tron

compile-solidity:
	$(MAKE) -C contracts/solidity build

compile-solana:
	$(MAKE) -C contracts/solana build

compile-cosmwasm:
	$(MAKE) -C contracts/cosmwasm/counter build
	$(MAKE) -C contracts/cosmwasm/vault build
	$(MAKE) -C contracts/cosmwasm/ping-pong build

compile-tron:
	$(MAKE) -C contracts/tron build

setup-solidity:
	cd contracts/solidity && forge install foundry-rs/forge-std

setup-tron:
	$(MAKE) -C contracts/tron setup

# FORMAT
fmt:
	cargo fmt --all
	cd contracts/solidity && forge fmt

# TESTS
# Pass extra cargo/libtest args via ARGS, e.g.
#   make test-harness ARGS="-- --nocapture"
#   make test ARGS="-- --show-output --test-threads=1"
test:
	cargo test --workspace $(ARGS)

test-cosmwasm:
	cargo test -p cross-vm-cosmwasm $(ARGS)

test-solidity:
	cargo test -p cross-vm-solidity $(ARGS)

test-solana:
	cargo test -p cross-vm-solana $(ARGS)
	
test-tron:
	cargo test -p cross-vm-tron $(ARGS)

# Example integration tests (cross-VM flows + the property-testing harness).
test-examples:
	cargo test -p cross-vm-tests $(ARGS)

# Harness suite without the opt-in modes: scenario / rstest matrices + runner mechanics.
test-harness:
	cargo test -p cross-vm-tests --test harness $(ARGS)

# Just the hand-written cross-VM flow tests.
test-cross-vm:
	cargo test -p cross-vm-tests --test cross_vm $(ARGS)

# Opt-in harness modes, each behind its own feature (the scenario tests run regardless).
test-fuzz:
	cargo test -p cross-vm-tests --test harness --features fuzz $(ARGS)

test-invariant:
	cargo test -p cross-vm-tests --test harness --features invariant $(ARGS)

test-endurance:
	cargo test -p cross-vm-tests --test harness --features endurance $(ARGS)

# Endurance with a live Base Sepolia chain added over RPC (needs network + a funded ON_CHAIN_WALLET
# mnemonic in .env). `rpc-endurance` injects the `"base"` chain into the shared counter setup, so
# the whole counter suite runs live; filter to the endurance test to avoid live matrix/fuzz runs.
test-rpc-endurance:
	cargo test -p cross-vm-tests --test harness --features "endurance rpc-endurance" $(ARGS) -- counter_endurance_mode --nocapture

# Harness suite with every mode enabled.
test-harness-all:
	cargo test -p cross-vm-tests --test harness --features "fuzz invariant endurance" $(ARGS)

# Per-VM example crates (single-VM Counter harness, driven three ways: harness / config / CLI).
test-examples-evm:
	cargo test -p evm-tests $(ARGS)

test-examples-cosmos:
	cargo test -p cosmos-tests $(ARGS)

test-examples-solana:
	cargo test -p solana-tests $(ARGS)

test-examples-tvm:
	cargo test -p tvm-tests $(ARGS)

test-examples-all: test-examples test-examples-evm test-examples-cosmos test-examples-solana test-examples-tvm