.PHONY: init run

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

run:
	npm run tauri dev
