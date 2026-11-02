.PHONY: init dev build run

# Catalog opened by dev/run (path to catalog.db). Default = same catalog the Tauri app uses.
# Override: make dev CATALOG=/path/to/catalog.db
CATALOG ?= $(HOME)/Library/Application Support/com.andrejvysny.darkroom/catalog.db

# Install deps: libheif (HEIF decode, macOS/Linux), npm packages, pinned Rust toolchain + crates.
# macOS: stage dylibs into src-tauri/frameworks (tauri-build validates them at compile time).
init:
	@if [ "$$(uname)" = "Darwin" ]; then \
		command -v brew >/dev/null || { echo "Homebrew required"; exit 1; }; \
		brew list libheif >/dev/null 2>&1 || brew install libheif; \
	elif command -v apt-get >/dev/null; then \
		sudo apt-get install -y libheif-dev libde265-dev; \
	fi
	@command -v rustup >/dev/null || { echo "rustup required: https://rustup.rs"; exit 1; }
	rustup show active-toolchain || rustup toolchain install
	npm install
	cargo fetch --locked
	bash scripts/macos-bundle-dylibs.sh stage

# Dev: native Rust UI app (apps/darkroom, egui/wgpu), debug build, with catalog.
dev:
	cargo run -p darkroom-native -- --catalog "$(CATALOG)"

# Release binary -> target/release/darkroom-native
build:
	cargo build --release -p darkroom-native

# Run the binary produced by `make build`, with catalog.
run:
	./target/release/darkroom-native --catalog "$(CATALOG)"
