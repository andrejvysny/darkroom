# Migration compatibility checks

`cargo xtask parity` checks the reviewed legacy command registry, emitted event topics,
screen references, browser settings and source anchors against `parity/*.json`.
Each entry identifies its native owner, observable behavior, implementation task and
test reference or explicit coverage gap. A referenced test is supporting evidence,
not proof that the entire command or its future native replacement is covered.

The targeted scanners are not Rust/TypeScript parsers. To catch alternate API syntax,
unusually named widgets, listener changes and new files, `source-index.json` also fences
every Rust/TypeScript source file under `src/` and `src-tauri/src/` by SHA-256. Any edit,
addition or removal requires reviewing its parity entries. This intentionally includes
nonbehavioral changes. It does not prove payload or semantic equivalence.

After reviewing changed behavior, ownership and test references, print candidate digests
with `cargo xtask parity-sources > /tmp/darkroom-source-index.json`. Inspect the diff
against `tests/migration/source-index.json` before replacing that file. CI only checks;
it never refreshes the reference automatically. Keep task progress in Plane.

## Synthetic catalogs

```sh
cargo run -p core-library --example migration_fixture -- /tmp/darkroom-fixture
cargo test -p core-library --test migration_baseline
```

The output directory must not exist. The generator creates schema-25 SQLite catalogs,
stored PV5/PV6 edits, a schema-1 sidecar, presets, snapshots, pairs, people and merge
provenance. `copied.db` uses `VACUUM INTO` to include live WAL contents. `corrupt.db`
and `newer.db` exercise refusal. The JPEG is a tiny valid synthetic image; CR3/EXR
files are deliberately invalid placeholders. These fixtures test persistence and
current parameter decoding, not historical photographic rendering. Paths point inside
the generated directory; moving it requires relinking. No installed catalog is opened.

## Browser reference captures

```sh
npm run e2e:mock
# Use an existing Chrome installation instead of Playwright's downloaded Chromium:
DARKROOM_E2E_CHANNEL=chrome npm run e2e:mock
```

`migration-baseline.spec.ts` attaches screenshots and viewport/DPR metadata under
Playwright's ignored `test-results/` directory. It exercises existing React UI using
the development IPC mock. These captures do not establish native backend, GPU,
latency, memory, accessibility or physical HDR acceptance. No imports, exports,
AI model downloads or merge jobs are started by the capture test.

## Source provenance

The legacy reference is Darkroom `5d75e37cec4cc70a828f2267a6dcfcc4823bedbe`, a
descendant of hardened `83fe690ac18eda42c2846cd0e4dd17b696948248`. The architectural
donor is [LightCraft at 5194cc2](https://github.com/storytold/lightcraft/tree/5194cc20dea06d725079ac162c6912fc1f0d3a78):
`docs/architecture.md`, `apps/lightcraft/src/main.rs`, `crates/ui-egui/src/services.rs`,
`crates/engine/src/cmd.rs`, `crates/develop/src/controls.rs`, `crates/ui-egui/src/docking.rs`
and `xtask/src/layers.rs`. Its code is MIT/Apache-2.0; separate brand terms apply.
No donor code, assets or dependency forks are copied by these compatibility tools.

## Native workflow measurement (macOS)

The opt-in specs in `e2e/tests/native/` require the real `e2e-testing` Tauri binary,
Vite on port 1420, and the committed CR3 copied into the isolated profile. They refuse
writes unless `app.getIdentifier()` is `com.andrejvysny.darkroom.baseline.20261030`
and the real path of `appDataDir()` equals
`/private/tmp/darkroom-native-baseline-profile-20261030`. Create that disposable profile
from the synthetic copied catalog; link only that dedicated bundle's Application Support
path to it. Never point it at an installed catalog. Put the CR3 at
`photos/reference.CR3` and create `exports/` under that profile. The specs reset this
copied photo's edits, index its copied directory, restore its rating, and export there.

Build the backend with a distinct identity, then launch that executable with Vite running:

```sh
TAURI_CONFIG='{"identifier":"com.andrejvysny.darkroom.baseline.20261030","productName":"Darkroom Baseline"}' \
  cargo build -p darkroom --features e2e-testing
DARKROOM_NATIVE_BASELINE=1 DARKROOM_BASELINE_PID=<native-process-id> \
  DARKROOM_BASELINE_BUILD='<source SHA, toolchain, wgpu, profile>' \
  npm run e2e:real -- e2e/tests/native/
```

Use a fresh webview load after changing the observer module: Vite hot replacement can
otherwise leave different module instances in the application and measurement harness.
The typed observer snapshots request identity before IPC and records its returned bytes.
Each accepted Stage paint must match a unique completed response byte-for-byte, use the
current measurement token and match the requested parameters. Stale or ambiguous responses
cannot satisfy edit latency. Paint time is recorded immediately after `putImageData`;
response retention and earlier instrumentation still perturb timings and process RSS.
Synthetic pointer events bypass pointer capture only in the test. Measurements include
bridge dispatch overhead and exclude compositor/display latency. First navigation is not
an OS-cache-cold measurement. Reopens return to the same photo, not a photo-switch test.
Native JSON, screenshots and exact frame request identities are Playwright attachments.
Physical input, resize, quit/reopen and HDR require separate native checks.

## Large-image resource probe

```sh
cargo build -p core-pipeline --example migration_resources
/usr/bin/time -l target/debug/examples/migration_resources 32mp
/usr/bin/time -l target/debug/examples/migration_resources 60mp --masked
/usr/bin/time -l target/debug/examples/migration_resources pano --masked
```

Run all three cases with and without `--masked`, each in a separate process, and retain
stdout/stderr. This uses synthetic linear input and the real GPU pipeline. It reports
preparation/render timings, process RSS, CPU input bytes and nominal mask texture bytes.
It does not measure GPU residency, photo quality or steady-state throughput. Debug build,
shader cache, hardware and preview dimensions must accompany reported results. Timings
and acceptance decisions belong in Plane, not this build guide.

## Native stack foundation

The exact upstream family is Rust 1.95.0, egui/egui-winit/egui-wgpu 0.37.0,
winit 0.30.13 and wgpu 30.0.1. The latter exposes explicit surface color-space
negotiation needed by the native presentation seam; see the
[upstream API](https://docs.rs/wgpu/30.0.1/wgpu/type.SurfaceConfiguration.html).
SQLite versions remain pinned independently, release panics unwind, and macOS deployment
continues to target 12.0. The target setting is not proof of runtime support on a macOS12
machine or of the deployment targets inside third-party binary libraries.

```sh
cargo build --locked -p darkroom-native
cargo run --locked -p darkroom-native -- --gpu-probe
cargo run --locked -p darkroom-native
cargo xtask layers
cargo test --locked -p xtask
npm run e2e:check-native
```

`darkroom-native` is an independent stack qualification executable. Its headless GPU probe
runs a tiny synthetic develop image and an egui render pass on the same device/queue.
Its application-owned winit window negotiates a supported SDR surface, renders egui,
handles resize/zero-size surfaces and recovers outdated/lost surfaces. Neither command
opens a catalog. The complete library/editor and physical HDR acceptance are subsequent
migration work, not features of this executable.

`xtask layers` consumes locked, all-features cargo metadata without filtering the host
target. It checks transitive dependencies, including renamed, development, build and
platform-specific edges, and rejects missing declared workspace dependencies in the
resolved graph. Core packages cannot reach GUI crates, ui-egui cannot reach SQL/native
service crates, and the native executable cannot reach Tauri. Cycles and multiple wgpu package identities fail, including same-major or
same-version crates from different sources. Metadata cannot detect direct `std::fs` calls or prove thread ownership;
those boundaries also require source review. Synthetic negative graphs exercise these
failures without modifying manifests or the installed application.

The CI native-host matrix builds this package independently on macOS arm64, macOS x86_64
and Windows x64. Local build evidence must identify which targets actually executed;
adding a workflow is not a pass on those runners. [NOTICE.md](../../NOTICE.md) records
upstream/native/model provenance and remaining distribution decisions.

See [native platform builds](../../docs/native-platform-builds.md) for pinned macOS 12
codecs, recursive deployment checks, Rosetta execution and Windows MSVC cross linking.
