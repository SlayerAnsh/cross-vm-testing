.PHONY: build setup

# Copy the Solidity example contracts and compile them with tronbox (TRON's tronc).
# pnpm install is idempotent, so `make compile` from the repo root works on a fresh checkout
# (first run fetches tronbox + the tron solc binary, so it needs network).
build:
	pnpm install
	pnpm compile

# Install the JS toolchain (tronbox) without compiling.
setup:
	pnpm install
