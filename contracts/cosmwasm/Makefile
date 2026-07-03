.PHONY: build

# Build every contract in this workspace with the CosmWasm optimizer Docker image, emitting MVP
# wasm (bulk-memory off) into ./artifacts/ named per crate (counter.wasm, ping_pong.wasm,
# vault.wasm) plus checksums.txt.
#
# Why the repo root is mounted instead of just this workspace: the counter/vault crates carry
# optional `cross-vm-*` path deps into ../../crates (host-test derives, off for the wasm build).
# Cargo loads those manifests even with the feature off, so the optimizer must see the repo root.
# We mount it at /code and point the optimizer at this workspace subdir; the `../../crates/...`
# deps then resolve inside the mount. Artifacts are emitted to <repo>/artifacts by the optimizer,
# then relocated here so the cosmwasm wasm lives beside its crates.
#
# RUSTUP_TOOLCHAIN pins the image's own toolchain (queried from the image) so the repo-root
# rust-toolchain.toml (channel = stable) does not shadow the wasm-enabled toolchain baked into the
# image. Override the image for the official one:
#   make build OPTIMIZER_IMAGE=cosmwasm/optimizer:0.16.1
OPTIMIZER_IMAGE ?= cosmwasm-optimizer-clang
REPO_ROOT := $(abspath $(CURDIR)/../..)
IMAGE_TOOLCHAIN := $(shell docker run --rm --entrypoint rustup $(OPTIMIZER_IMAGE) show active-toolchain 2>/dev/null | awk '{print $$1}')

build:
	docker run --rm \
		-e RUSTUP_TOOLCHAIN=$(IMAGE_TOOLCHAIN) \
		-v "$(REPO_ROOT)":/code \
		--mount type=volume,source=cosmwasm_cache,target=/code/target \
		--mount type=volume,source=registry_cache,target=/usr/local/cargo/registry \
		$(OPTIMIZER_IMAGE) ./contracts/cosmwasm
	mkdir -p artifacts
	mv "$(REPO_ROOT)"/artifacts/*.wasm "$(REPO_ROOT)"/artifacts/checksums.txt artifacts/
	rmdir "$(REPO_ROOT)"/artifacts 2>/dev/null || true
