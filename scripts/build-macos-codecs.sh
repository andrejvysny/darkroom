#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=macos-codecs.env
source "$script_dir/macos-codecs.env"
arch=''
prefix=''
while (($#)); do
  case "$1" in
    --arch) arch=${2:?missing architecture}; shift 2 ;;
    --prefix) prefix=${2:?missing prefix}; shift 2 ;;
    *) echo "usage: $0 --arch arm64|x86_64 --prefix ABSOLUTE_PATH" >&2; exit 2 ;;
  esac
done
case "$arch" in arm64|x86_64) ;; *) echo 'Invalid architecture' >&2; exit 2 ;; esac
case "$prefix" in /*) ;; *) echo 'Prefix must be absolute' >&2; exit 2 ;; esac
if [[ -e "$prefix" ]]; then echo "Prefix already exists: $prefix" >&2; exit 2; fi
: "${CODECS_WORK_ROOT:=${TMPDIR:-/tmp}/darkroom-macos-codecs}"
: "${MESON:=meson}" "${NASM:=nasm}" "${CMAKE:=cmake}" "${NINJA:=ninja}" "${JOBS:=4}"
for tool in "$MESON" "$CMAKE" "$NINJA" curl git shasum pkg-config xcrun; do
  command -v "$tool" >/dev/null || { echo "Missing build tool: $tool" >&2; exit 2; }
done
if [[ $arch == x86_64 ]]; then
  command -v "$NASM" >/dev/null || { echo "Missing build tool: $NASM" >&2; exit 2; }
fi
mkdir -p "$CODECS_WORK_ROOT" "$(dirname "$prefix")"
work=$(mktemp -d "$CODECS_WORK_ROOT/build-${arch}.XXXXXX")
mkdir "$prefix"
mkdir -p "$work/sources" "$work/build"
printf 'Build directory: %s\nPrefix: %s\n' "$work" "$prefix"
export MACOSX_DEPLOYMENT_TARGET=12.0
export PKG_CONFIG_LIBDIR="$prefix/lib/pkgconfig"
export PKG_CONFIG_PATH="$prefix/lib/pkgconfig"
export CMAKE_PREFIX_PATH="$prefix"
PATH="$(dirname "$(command -v "$NINJA")"):$(dirname "$(command -v "$NASM" 2>/dev/null || command -v "$NINJA")"):$PATH"
export PATH

fetch() {
  local name=$1 url=$2 expected=$3 archive="$work/$1.tar.gz" actual
  curl --fail --location --retry 3 --output "$archive" "$url"
  actual=$(shasum -a 256 "$archive" | awk '{print $1}')
  [[ $actual == "$expected" ]] || { echo "SHA-256 mismatch: $name" >&2; exit 1; }
  mkdir "$work/sources/$name"
  tar -xzf "$archive" -C "$work/sources/$name" --strip-components=1
}

cmake_configure() {
  local source=$1 build=$2; shift 2
  "$CMAKE" -S "$source" -B "$build" -G Ninja \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$prefix" \
    -DCMAKE_INSTALL_LIBDIR=lib -DCMAKE_INSTALL_RPATH='@loader_path' \
    -DCMAKE_SYSTEM_NAME=Darwin -DCMAKE_SYSTEM_PROCESSOR="$arch" \
    -DCMAKE_OSX_ARCHITECTURES="$arch" -DCMAKE_OSX_DEPLOYMENT_TARGET=12.0 \
    -DCMAKE_PREFIX_PATH="$prefix" -DCMAKE_IGNORE_PREFIX_PATH=/opt/homebrew\;/usr/local \
    "$@"
}
cmake_install() {
  "$CMAKE" --build "$1" --parallel "$JOBS"
  "$CMAKE" --install "$1"
}

build_vmaf() {
  local cpu_family=$arch
  [[ $arch == arm64 ]] && cpu_family=aarch64
  cat > "$work/meson-cross.txt" <<CROSS
[binaries]
c = '/usr/bin/clang'
cpp = '/usr/bin/clang++'
ar = '/usr/bin/ar'
strip = '/usr/bin/strip'
pkg-config = '$(command -v pkg-config)'
nasm = '$(command -v "$NASM" 2>/dev/null || command -v true)'
[host_machine]
system = 'darwin'
cpu_family = '$cpu_family'
cpu = '$cpu_family'
endian = 'little'
[built-in options]
c_args = ['-arch', '$arch', '-mmacosx-version-min=12.0']
cpp_args = ['-arch', '$arch', '-mmacosx-version-min=12.0']
c_link_args = ['-arch', '$arch', '-mmacosx-version-min=12.0']
cpp_link_args = ['-arch', '$arch', '-mmacosx-version-min=12.0']
CROSS
  "$MESON" setup "$work/build/vmaf" "$work/sources/vmaf/libvmaf" \
    --cross-file "$work/meson-cross.txt" --prefix "$prefix" --libdir lib \
    --buildtype release --default-library shared
  "$MESON" compile -C "$work/build/vmaf" -j "$JOBS"
  "$MESON" install -C "$work/build/vmaf"
  install_name_tool -id @rpath/libvmaf.3.dylib "$prefix/lib/libvmaf.3.dylib"
}

build_aom() {
  git clone --depth 1 --branch "$AOM_TAG" "$AOM_URL" "$work/sources/aom"
  [[ $(git -C "$work/sources/aom" rev-parse HEAD) == "$AOM_REVISION" ]] || {
    echo 'AOM revision mismatch' >&2; exit 1;
  }
  local cpu=$arch
  [[ $arch == arm64 ]] && cpu=arm64
  cmake_configure "$work/sources/aom" "$work/build/aom" \
    -DAOM_TARGET_CPU="$cpu" -DCONFIG_TUNE_VMAF=1 -DBUILD_SHARED_LIBS=ON \
    -DENABLE_DOCS=OFF -DENABLE_EXAMPLES=OFF -DENABLE_TESTS=OFF -DENABLE_TOOLS=OFF \
    -DCMAKE_ASM_NASM_COMPILER="$(command -v "$NASM" 2>/dev/null || command -v true)"
  cmake_install "$work/build/aom"
}

build_x265() {
  local source="$work/sources/x265/source" build="$work/build/x265"
  mkdir -p "$build/8bit"
  cmake_configure "$source" "$build/10bit" -DHIGH_BIT_DEPTH=ON -DEXPORT_C_API=OFF \
    -DENABLE_SHARED=OFF -DENABLE_CLI=OFF -DENABLE_HDR10_PLUS=ON
  "$CMAKE" --build "$build/10bit" --parallel "$JOBS"
  cp "$build/10bit/libx265.a" "$build/8bit/libx265_main10.a"
  cmake_configure "$source" "$build/12bit" -DHIGH_BIT_DEPTH=ON -DMAIN12=ON \
    -DEXPORT_C_API=OFF -DENABLE_SHARED=OFF -DENABLE_CLI=OFF
  "$CMAKE" --build "$build/12bit" --parallel "$JOBS"
  cp "$build/12bit/libx265.a" "$build/8bit/libx265_main12.a"
  cmake_configure "$source" "$build/8bit" -DLINKED_10BIT=ON -DLINKED_12BIT=ON \
    "-DEXTRA_LINK_FLAGS=-L$build/8bit" '-DEXTRA_LIB=x265_main10.a;x265_main12.a' \
    -DENABLE_SHARED=ON -DENABLE_CLI=OFF
  cmake_install "$build/8bit"
}

fetch libheif "$LIBHEIF_URL" "$LIBHEIF_SHA256"
fetch libde265 "$LIBDE265_URL" "$LIBDE265_SHA256"
fetch webp "$WEBP_URL" "$WEBP_SHA256"
fetch x265 "$X265_URL" "$X265_SHA256"
fetch vmaf "$VMAF_URL" "$VMAF_SHA256"
build_vmaf
build_aom
cmake_configure "$work/sources/libde265" "$work/build/libde265" -DBUILD_SHARED_LIBS=ON -DENABLE_DECODER=OFF
cmake_install "$work/build/libde265"
cmake_configure "$work/sources/webp" "$work/build/webp" -DBUILD_SHARED_LIBS=ON \
  -DWEBP_BUILD_ANIM_UTILS=OFF -DWEBP_BUILD_CWEBP=OFF -DWEBP_BUILD_DWEBP=OFF \
  -DWEBP_BUILD_GIF2WEBP=OFF -DWEBP_BUILD_IMG2WEBP=OFF -DWEBP_BUILD_VWEBP=OFF \
  -DWEBP_BUILD_WEBPINFO=OFF -DWEBP_BUILD_WEBPMUX=OFF -DWEBP_BUILD_EXTRAS=OFF
cmake_install "$work/build/webp"
build_x265
cmake_configure "$work/sources/libheif" "$work/build/libheif" \
  -DBUILD_SHARED_LIBS=ON -DENABLE_PLUGIN_LOADING=OFF -DWITH_LIBDE265=ON -DWITH_X265=ON \
  -DWITH_AOM_DECODER=ON -DWITH_AOM_ENCODER=ON -DWITH_LIBSHARPYUV=ON \
  -DWITH_DAV1D=OFF -DWITH_OpenH264_DECODER=OFF -DWITH_RAV1E=OFF -DWITH_SvtEnc=OFF \
  -DWITH_X264=OFF -DWITH_EXAMPLES=OFF -DWITH_GDK_PIXBUF=OFF -DBUILD_TESTING=OFF 2>&1 | tee "$work/heif-config.log"
for backend in libde265 x265 aomdec aomenc; do
  grep -F "Compiling '$backend' as built-in backend" "$work/heif-config.log" >/dev/null || {
    echo "Missing libheif backend: $backend" >&2; exit 1;
  }
done
grep -F "libsharpyuv: found" "$work/heif-config.log" >/dev/null || {
  echo "Missing libheif sharpyuv" >&2; exit 1;
}
cmake_install "$work/build/libheif"
for lib in heif de265 aom sharpyuv x265 vmaf; do
  file "$prefix/lib/lib$lib.dylib" | tee "$work/$lib-file.log"
  grep -F "$arch" "$work/$lib-file.log" >/dev/null
  xcrun vtool -show-build "$prefix/lib/lib$lib.dylib" | tee "$work/$lib-build-version.log"
  grep -E "minos +12\.0$" "$work/$lib-build-version.log" >/dev/null
  while IFS= read -r dependency; do
    case "$dependency" in
      @rpath/*|@loader_path/*|/usr/lib/*|/System/Library/*) ;;
      *) echo "Non-relocatable dependency in lib$lib: $dependency" >&2; exit 1 ;;
    esac
  done < <(otool -L "$prefix/lib/lib$lib.dylib" | tail -n +2 | awk '{print $1}')
done
if /usr/bin/arch "-$arch" /usr/bin/true 2>/dev/null; then
  cat > "$work/codec-probe.c" <<'PROBE'
#include <libheif/heif.h>
#include <x265.h>
int main(void) {
  if (!heif_have_decoder_for_format(heif_compression_HEVC) ||
      !heif_have_encoder_for_format(heif_compression_HEVC) ||
      !heif_have_decoder_for_format(heif_compression_AV1) ||
      !heif_have_encoder_for_format(heif_compression_AV1)) return 1;
  if (!x265_api_get(8) || !x265_api_get(10) || !x265_api_get(12)) return 2;
  return 0;
}
PROBE
  /usr/bin/clang -arch "$arch" -mmacosx-version-min=12.0 \
    -I"$prefix/include" "$work/codec-probe.c" -L"$prefix/lib" -lheif -lx265 \
    -Wl,-rpath,"$prefix/lib" -o "$work/codec-probe"
  /usr/bin/arch "-$arch" "$work/codec-probe"
  mkdir -p "$prefix/bin"
  install -m 755 "$work/codec-probe" "$prefix/bin/darkroom-codec-probe"
else
  echo "Cannot execute $arch probe on this host; runtime qualification remains required" >&2
fi
# Keep the exact source notices beside artifacts for the later packaging step.
for component in libheif libde265 webp x265 vmaf aom; do
  notices="$prefix/share/licenses/$component"
  mkdir -p "$notices"
  for notice in COPYING LICENSE PATENTS AUTHORS; do
    if [[ -f "$work/sources/$component/$notice" ]]; then
      cp "$work/sources/$component/$notice" "$notices/$notice"
    fi
  done
done
cp "$script_dir/macos-codecs.env" "$prefix/share/licenses/macos-codecs.env"
printf 'Codec build complete: %s\n' "$prefix"
