# Source and asset provenance

This inventory accompanies the source and build configuration. It does not grant rights
beyond each upstream license or replace the notices required by a distributed bundle.
The workspace declares MIT in Cargo metadata, but no authoritative application LICENSE
text was present in the reference checkout. The migration does not invent copyright
holders, relicense retained material, or authorize a release.

## Native application stack

The application uses upstream crates, without donor forks:

| Component | Exact version | Registry license expression | Upstream |
| --- | --- | --- | --- |
| egui, egui-winit, egui-wgpu | 0.37.0 | MIT OR Apache-2.0 | [emilk/egui](https://github.com/emilk/egui/tree/0.37.0) |
| wgpu | 30.0.1 | MIT OR Apache-2.0 | [gfx-rs/wgpu](https://github.com/gfx-rs/wgpu/tree/v30.0.1) |
| winit | 0.30.13 | Apache-2.0 | [rust-windowing/winit](https://github.com/rust-windowing/winit/tree/v0.30.13) |

LightCraft at [5194cc20dea06d725079ac162c6912fc1f0d3a78](https://github.com/storytold/lightcraft/tree/5194cc20dea06d725079ac162c6912fc1f0d3a78)
is an architectural reference. Its source is offered under MIT/Apache-2.0, with separate
brand terms. No LightCraft source, assets, fonts, branding or dependency forks were copied
into the native foundation. Darkroom uses its own name and upstream egui default fonts;
those font assets retain the licenses distributed with egui/epaint_default_fonts.
Unmodified font notices are preserved in [third_party/egui-fonts](third_party/egui-fonts/README.md).

## Retained image and native dependencies

The following expressions are declarations from the resolved crate manifests, not a
complete license assessment of codecs, native binaries or their transitive dependencies.
Exact package identities and checksums live in [Cargo.lock](Cargo.lock). Generate the
full cross-platform inventory with `cargo metadata --locked --all-features --format-version 1`;
retain the actual license files for the versions and native artifacts in each release.

| Component | Locked version | Crate license expression | Rust adapter |
| --- | --- | --- | --- |
| rawler | 0.8.0 | LGPL-2.1 | `crates/core-raw` |
| libheif-rs / libheif-sys | 2.7.0 / 5.3.0+1.23.0 | MIT / MIT | `crates/core-raw/src/heif.rs` |
| exr | 1.74.0 | BSD-3-Clause | `crates/core-raw/src/hdr_file.rs` |
| ort | 2.0.0-rc.12 | MIT OR Apache-2.0 | `crates/core-analyze` |
| rusqlite / rusqlite_migration | 0.39.0 / 2.5.0 | MIT / Apache-2.0 | `crates/core-db` |

The libheif Rust wrapper's MIT declaration does not describe libheif or its codecs.
The pinned macOS source closure is recorded in
[scripts/macos-codecs.env](scripts/macos-codecs.env): libheif 1.23.5, libde265 1.1.3,
AOM 3.15.1, WebP/sharpyuv 1.6.0, x265 4.3 and VMAF 3.2.1. Its builder retains each
source's COPYING/LICENSE/PATENTS/AUTHORS files when present under `share/licenses/`.
These include libheif/libde265 LGPL notices, x265 GPL notices, AOM/WebP patent notices
and VMAF's BSD patent license. Source/object distribution obligations and inclusion of
these notices in final installers remain part of distribution acceptance.
The system/bundled library closure, its upstream notices, ORT execution providers and
platform frameworks must be assessed from the actual built artifacts. The macOS closure
is staged by [scripts/macos-bundle-dylibs.sh](scripts/macos-bundle-dylibs.sh).
HEIF remains platform-gated; Windows uses the existing typed unsupported adapter.

## Tone-curve reference data

[base_curve_ref.rs](crates/core-pipeline/src/base_curve_ref.rs) identifies its 1025 values
as the `adobe_camera_raw_default_curve` table from RawTherapee's `rtengine/dcp.cc`.
The copied source revision and permission history are not recorded there. RawTherapee
publishes [its source](https://github.com/RawTherapee/RawTherapee/blob/dev/rtengine/dcp.cc)
under [GPLv3](https://github.com/RawTherapee/RawTherapee/blob/dev/LICENSE).
The local comment about fitting another LUT does not establish permission or resolve
license obligations for the retained reference data. This migration preserves the data
and pixel behavior; distribution requires an explicit provenance/license resolution.

## Model artifacts

[models.rs](crates/core-analyze/src/models.rs) is the canonical artifact inventory: exact
URLs, relative names and SHA-256 digests. Digests verify bytes; they do not grant a license.
Several URLs use mutable `main` revisions, so release provenance also needs the source
revision, export history and applicable model license for those exact bytes.

| Artifact family | Existing source declaration | Provenance consideration |
| --- | --- | --- |
| D-FINE-M | Apache-2.0 | ONNX export and original weights require their applicable notices. |
| Florence-2-base-ft | MIT | Includes separate encoder/decoder/embedding/tokenizer artifacts. |
| MegaDetector v5a | MIT | Community ONNX export is fetched from release v0.2.0. |
| MobileCLIP-S1 | MIT | Vision/text exports and tokenizer are separate artifacts. |
| MobileSAM | Apache-2.0 | Export URLs pin revision `0d3b403339b4674a82493d5e97964dd78089ddc8`. |
| SAM ViT-B / ViT-L | No license assertion in registry entries | Verify original weights and each third-party export. |
| SCRFD / ArcFace buffalo_l | Non-commercial pretrained weights | InsightFace policy restricts pretrained models to non-commercial research; app-local use alone does not establish permission. |
| PMRID denoiser | Apache-2.0 assertion in denoise source | The embedded ONNX export needs exact upstream weight/export provenance and notices. |

InsightFace's [model-zoo policy](https://github.com/deepinsight/insightface/tree/master/model_zoo)
and [project policy](https://github.com/deepinsight/insightface) distinguish code licensing
from pretrained model licensing. The migration does not replace, download or broaden use
of these models. Product/distribution approval must resolve the restricted weights and
unverified export provenance before shipping the relevant features.

## Test photographs and other assets

The downloadable RAW corpus has per-artifact origin and hashes in
[tests/corpus/manifest.toml](tests/corpus/manifest.toml); see its
[CC0 acquisition instructions](tests/corpus/README.md). These downloaded bytes and local
photo libraries are not included as native application assets. The migration fixture
creates synthetic data; the native baseline copies the committed CR3 without modifying
its original. Existing application icons/assets require their own origin/permission
records; the workspace MIT declaration alone does not establish their provenance.
