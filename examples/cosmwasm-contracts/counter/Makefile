.PHONY: build

# Build a chain-deployable contract with the CosmWasm optimizer Docker image. It pins an older
# rustc (bulk-memory off by default) and runs wasm-opt, producing MVP wasm the VM accepts; a
# plain host `cargo build` with current rustc emits post-MVP bulk-memory ops chains reject.
# Output: artifacts/counter.wasm. Override the image for the official one:
#   make build OPTIMIZER_IMAGE=cosmwasm/optimizer:0.16.1
OPTIMIZER_IMAGE ?= cosmwasm-optimizer-clang

build:
	docker run --rm -v "$(CURDIR)":/code \
		--env RUSTFLAGS="-C link-arg=-s target-feature=-bulk-memory" \
		--mount type=volume,source=counter_cache,target=/code/target \
		--mount type=volume,source=registry_cache,target=/usr/local/cargo/registry \
		$(OPTIMIZER_IMAGE)
