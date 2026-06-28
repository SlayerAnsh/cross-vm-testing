.PHONY: compile compile-solidity compile-solana compile-cosmwasm setup-solidity

compile: compile-solidity compile-solana compile-cosmwasm

compile-solidity:
	$(MAKE) -C examples/solidity-contracts build

compile-solana:
	$(MAKE) -C examples/solana-contracts build

compile-cosmwasm:
	$(MAKE) -C examples/cosmwasm-contracts/counter build

setup-solidity:
	cd examples/solidity-contracts && forge install foundry-rs/forge-std
