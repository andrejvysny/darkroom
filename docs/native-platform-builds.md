# Native platform qualification

The native foundation is the `darkroom-native` Cargo package. It builds independently
of Tauri and does not open a catalog. Its `--gpu-probe` exercises image processing and
egui using the same GPU device and queue. Complete application packaging is tracked
separately in DARKROOM-153.

## macOS codecs and deployment target

The native executable and every retained codec must support the macOS 12 target.
Homebrew bottles can require a newer OS even when Rust declares `minos 12.0`.
Build the exact source versions in [macos-codecs.env](../scripts/macos-codecs.env):
libheif 1.23.5, libde265 1.1.3, AOM 3.15.1, WebP/sharpyuv 1.6.0, x265 4.3 and
VMAF 3.2.1. Archives are checked by SHA-256; AOM is checked against its exact Git revision.

Prerequisites: Xcode command-line tools, CMake, Ninja, pkg-config, Git, curl and Meson.
Intel builds also require NASM. Meson 1.7.2 and NASM 2.16.03 were locally qualified.
Meson can run from an isolated virtual environment:

```sh
python3 -m venv /tmp/darkroom-codec-tools
/tmp/darkroom-codec-tools/bin/pip install --only-binary=:all: --no-deps meson==1.7.2
MESON=/tmp/darkroom-codec-tools/bin/meson \
  bash scripts/build-macos-codecs.sh --arch arm64 --prefix "$PWD/target/codecs-arm64"
# On an Intel host, use --arch x86_64 and a separate prefix.
```

The prefix must not already exist. `CODECS_WORK_ROOT` selects the temporary source/build
root; `JOBS`, `CMAKE`, `NINJA`, `MESON` and `NASM` select build tools/concurrency.
The builder retains all six dylibs, HEVC/AV1 encoder and decoder backends, AOM's VMAF
support, sharpyuv and x265's 8/10/12-bit APIs. It checks architecture and deployment
metadata, validates relocatable install names and runs a capability probe when the target
architecture can execute. Cross execution on Apple Silicon requires Rosetta. Original
source notices are retained under the prefix's `share/licenses/` for later packaging.

Select the isolated prefix for Cargo and staging:

```sh
export DARKROOM_CODEC_PREFIX="$PWD/target/codecs-arm64"
export PKG_CONFIG_PATH="$DARKROOM_CODEC_PREFIX/lib/pkgconfig"
export PKG_CONFIG_LIBDIR="$PKG_CONFIG_PATH"
export DYLD_LIBRARY_PATH="$DARKROOM_CODEC_PREFIX/lib"
cargo build --locked -p darkroom-native
cargo xtask macos-deployment target/debug/darkroom-native arm64
cargo run --locked -p darkroom-native -- --gpu-probe
bash scripts/macos-bundle-dylibs.sh stage
```

`macos-deployment` recursively checks each non-system Mach-O's architecture, minimum OS
and load-path resolution. It supports loader/executable paths, rpaths and
`DYLD_LIBRARY_PATH` overrides; unresolved or incompatible dependencies fail. It does not
certify symbol availability or runtime behavior on an untested OS. The staging script
checks the source closure before copying/signing and rewrites references to the bundled
stable names. `DARKROOM_DYLIB_STAGE` optionally selects an isolated staging directory.

CI uses the shared [macOS codec action](../.github/actions/macos-codecs/action.yml),
caches pinned source-built artifacts and executes the capability probe after cache
restoration. The native build matrix then audits the executable's full closure.

For Rosetta tests, macOS system tools can remove inherited `DYLD_*` variables. Set the
loader path explicitly after `arch`:

```sh
CARGO_TARGET_X86_64_APPLE_DARWIN_RUNNER="arch -x86_64 /usr/bin/env DYLD_LIBRARY_PATH=$DARKROOM_CODEC_PREFIX/lib" \
  cargo test --locked -p core-raw --target x86_64-apple-darwin --test heif_decode
```

Qualification bundles must also work with `DYLD_LIBRARY_PATH` unset and their libraries
inside `Contents/Frameworks`. Inspect the final copied executable with
`cargo xtask macos-deployment <bundle>/Contents/MacOS/darkroom-native <architecture>`.
macOS 12 hardware, physical Intel GPUs, signing and clean-install checks remain explicit
release/runtime acceptance gates.

## Windows MSVC cross linking from macOS

Use installed LLVM tools and an existing licensed MSVC CRT/Windows SDK sysroot containing
`crt/lib/x86_64`, `sdk/lib/um/x86_64` and `sdk/lib/ucrt/x86_64`. This recipe was qualified
with SDK 10.0.26100 and CRT 14.44.17.14. Rust 1.95.0's bundled LLD avoids a dependency
on matching the host's separately installed LLD and LLVM dylib versions.

```sh
export DARKROOM_WINDOWS_SYSROOT=/absolute/path/to/existing/sysroot
export DARKROOM_LLVM_BIN=/absolute/path/to/llvm/bin
export DARKROOM_RUST_LLD="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin/rust-lld"
mkdir -p target/platform-tools
cat > target/platform-tools/windows-linker <<'SH'
#!/bin/sh
exec "$DARKROOM_RUST_LLD" -flavor link \
  "/libpath:$DARKROOM_WINDOWS_SYSROOT/crt/lib/x86_64" \
  "/libpath:$DARKROOM_WINDOWS_SYSROOT/sdk/lib/um/x86_64" \
  "/libpath:$DARKROOM_WINDOWS_SYSROOT/sdk/lib/ucrt/x86_64" "$@"
SH
chmod +x target/platform-tools/windows-linker
CC_x86_64_pc_windows_msvc="$DARKROOM_LLVM_BIN/clang" \
AR_x86_64_pc_windows_msvc="$DARKROOM_LLVM_BIN/llvm-lib" \
CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER="$PWD/target/platform-tools/windows-linker" \
  cargo build --locked -p darkroom-native --target x86_64-pc-windows-msvc
```

The compiler selection retains BLAKE3's optimized Windows assembly and the production
Cargo feature set. Inspect the resulting PE architecture and DLL imports with
`llvm-readobj --file-headers --coff-imports`. Linking does not establish Windows GPU,
CRT availability, installer or interactive runtime acceptance. Native Windows CI builds
and later clean-machine tests retain those responsibilities.
