.PHONY: build build-tests

# Build only the deployable contracts under src/. The cross-VM Rust tests embed those
# artifacts (out/<Name>.sol/<Name>.json), and src/ has no forge-std dependency, so this
# stays green on a fresh checkout / CI without `forge install`. The test/*.t.sol files
# (which import forge-std) are skipped here; use `make build-tests` or `forge test` for them.
build:
	forge build --skip "*.t.sol"

# Full build including the forge test contracts (needs forge-std: `make -C ../.. setup-solidity`).
build-tests:
	forge build
