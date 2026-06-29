.PHONY: build build-anchor

# Build the program .so files the cross-VM tests embed (target/deploy/{counter,ping_pong,vault}.so).
# cargo build-sbf (from the Solana/Agave platform tools) compiles the cdylib programs directly,
# with no Anchor CLI and no anchor-cli/anchor-lang version handshake. The tests use raw instruction
# discriminators, so the Anchor IDL is not needed.
build:
	cargo build-sbf

# Full Anchor build (also generates the IDL). Needs the anchor CLI whose version matches the
# programs' anchor-lang (0.31.1); handy locally, not used by CI.
build-anchor:
	anchor build
