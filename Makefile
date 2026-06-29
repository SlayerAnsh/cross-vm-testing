.PHONY: compile compile-solidity compile-solana compile-cosmwasm setup-solidity fmt \
	test test-cosmwasm test-solidity test-solana test-examples test-harness test-cross-vm \
	test-fuzz test-invariant test-endurance test-harness-all

compile: compile-solidity compile-solana compile-cosmwasm

compile-solidity:
	$(MAKE) -C examples/solidity-contracts build

compile-solana:
	$(MAKE) -C examples/solana-contracts build

compile-cosmwasm:
	$(MAKE) -C examples/cosmwasm-contracts/counter build
	$(MAKE) -C examples/cosmwasm-contracts/vault build
	$(MAKE) -C examples/cosmwasm-contracts/ping-pong build

setup-solidity:
	cd examples/solidity-contracts && forge install foundry-rs/forge-std

# FORMAT
fmt:
	cargo fmt --all
	cd examples/solidity-contracts && forge fmt

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

# Example integration tests (cross-VM flows + the property-testing harness).
test-examples:
	cargo test -p cross-vm-integration-tests $(ARGS)

# Harness suite without the opt-in modes: scenario / rstest matrices + runner mechanics.
test-harness:
	cargo test -p cross-vm-integration-tests --test harness $(ARGS)

# Just the hand-written cross-VM flow tests.
test-cross-vm:
	cargo test -p cross-vm-integration-tests --test cross_vm $(ARGS)

# Opt-in harness modes, each behind its own feature (the scenario tests run regardless).
test-fuzz:
	cargo test -p cross-vm-integration-tests --test harness --features fuzz $(ARGS)

test-invariant:
	cargo test -p cross-vm-integration-tests --test harness --features invariant $(ARGS)

test-endurance:
	cargo test -p cross-vm-integration-tests --test harness --features endurance $(ARGS)

# Harness suite with every mode enabled.
test-harness-all:
	cargo test -p cross-vm-integration-tests --test harness --features "fuzz invariant endurance" $(ARGS)