# Photo Filter Studio

[![CI](https://github.com/juusolatva/photo-filter/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/juusolatva/photo-filter/actions/workflows/ci.yml)
[![Security](https://github.com/juusolatva/photo-filter/actions/workflows/security.yml/badge.svg?branch=main)](https://github.com/juusolatva/photo-filter/actions/workflows/security.yml)
[![Quality Gate Status](https://sonarcloud.io/api/project_badges/measure?project=juusolatva_photo-filter&metric=alert_status)](https://sonarcloud.io/summary/new_code?id=juusolatva_photo-filter)
[![Coverage](https://sonarcloud.io/api/project_badges/measure?project=juusolatva_photo-filter&metric=coverage)](https://sonarcloud.io/summary/new_code?id=juusolatva_photo-filter)
[![Rust](https://img.shields.io/badge/rust-2024%20edition-orange?logo=rust)](Cargo.toml)
[![License: MIT](https://img.shields.io/github/license/juusolatva/photo-filter)](LICENSE)

A native Rust photo editor for Linux and Windows, with a Phase 0 viewer/export spike and the Phase 1 edit model and basic pointwise renderer from [the implementation plan](PLAN.md).

For hands-on checks, use the [Linux and Windows test checklist](docs/manual-testing.md).

Implemented: background JPEG/PNG/WebP/TIFF loading, EXIF orientation, embedded ICC conversion to straight linear Rec.2020 in half-float storage, alpha-aware 2048px proxies, and a GPU basic-adjustment viewport with fit, pan and pointer-centered zoom. Opening another image discards stale decode results. Images exceeding the initial per-image 2 GiB estimated working budget are rejected with an error. Input files are capped at 512 MiB, and retained ICC/EXIF metadata at 16 MiB each; decoder limits also apply. These are allocation guards, not a hard process memory limit. Tagged grayscale profiles are converted using their original single-channel layout.

```sh
cargo run -p filter-app -- path/to/photo.jpg
cargo run -p filter-app -- --software-gpu path/to/photo.png
```

Use **Open image**, **Ctrl+O**, or drag a file onto the window. Drag to pan, scroll to zoom, and select **Fit** to reset the view. **100%** shows one source pixel per physical display pixel, with pixel-aligned panning; this scale survives window resizing and display scaling changes until you scroll or select Fit. Exposure ranges from −5 to +5 EV. Relative temperature/tint, brightness, contrast, highlights/shadows, whites/blacks, vibrance and saturation range from −1 to +1. All edits leave the source untouched; **Reset adjustments** returns these controls to neutral while preserving other edit sections.

At 100% zoom and above, visible regions progressively refine with
full-resolution source tiles. A background worker uploads one tile at a time on the
viewer's GPU device, while the proxy fills areas awaiting refinement. The cache holds
at most 63 source tiles of up to 512 × 512 pixels, with one-pixel filtering borders,
within a 128 MiB texture-payload budget (excluding driver overhead and in-flight work).
Very large visible source regions refine a central subset and retain the proxy elsewhere.
Tiles survive panning and adjustment changes; each draw uses the current basic settings,
and results from a previous image are discarded. No full-image GPU texture or region
readback is required. Spatial filtering remains pending.
Below 100%, the area-filtered proxy stays visible to avoid aliasing from minifying
single-level source tiles; filtered region rendering at intermediate scales remains pending.
Viewport bilinear interpolation uses explicit float math to avoid adapter-dependent
sampler weight precision near high-contrast tile boundaries.
All tiles derive sampling coordinates from the same physical viewport bounds,
including its window offset, rather than interpolating UVs over individual quads.
Tile uniform buffers and bind groups are reused across repaints, with parameter updates
batched through reusable staging memory. A failed tile leaves its area on the proxy while
other tiles continue refining. **Retry preview** frees the cache and retries failed tiles
without reopening the image. An unavailable worker is reported separately.

The adjustment panel shows a live 256-bin **RGB/luminance histogram** of the whole
edited preview, independent of pan or zoom. Hover it for channel clipping counts,
sample counts and transparency details. Fully transparent pixels are excluded;
partial alpha counts once, before checkerboard compositing or export flattening.
RGB clipping counts preview pixels with any sRGB channel at black or white, including
exact endpoints. Counts describe the proxy rather than full-resolution export pixels.
Older curves are dimmed and marked **Updating…** while the current edit is measured;
results from superseded edits/images cannot replace current data. Failed readback
offers **Retry histogram**. See [the histogram contract](docs/histogram.md).

Hover over the image for a **Cursor color** readout below the histogram: source
pixel coordinates, edited RGB/hex, Oklab and alpha. It measures the original
full-resolution pixel at every zoom level, so Fit/bilinear screen colors can differ.
RGB/hex use nearest 8-bit clipped sRGB; Oklab describes the same output color,
with alpha separate and no checkerboard, flattening or dithering. Transparent
pixels show no hidden color. Leaving the image clears the reading; older
cursor/edit/image results are discarded. **Retry color probe** recovers a failed
readback. Sampling is bounded and asynchronous on the viewer's device.
See [the cursor probe contract and manual checks](docs/probe.md).

**Undo** and **Redo** in the toolbar (**Ctrl+Z**, and **Ctrl+Shift+Z** or **Ctrl+Y**)
step through whole-edit history. A slider drag, a held key or a typed slider value
(applied on Enter or focus loss) is one step; preset application and **Reset adjustments**
are also steps. Changes settle once no pointer button or key is held. While a text field
has focus, the shortcuts are left to the field. Undo during an unfinished drag first
records it, so redo can return to it. Opening an image clears history. History keeps at most
100 steps and about 64 MiB of estimated edit data (relevant only for presets with large
unknown future filters), dropping the oldest first. Exports keep the settings they captured.

Under **Presets**, use **Load preset…** (or drop a `.pfpreset.json` file), choose sections, and **Apply selected sections**. Unsupported active effects leave the current edit intact with an explanation; unknown future filters are preserved and skipped. **Save preset…** captures the current adjustments and selected sections as versioned JSON with a native overwrite dialog and atomic publication. Imports and serialized saves are limited to 16 MiB. See [the preset workflow and merge contract](docs/presets.md).

Choose **PNG**, **JPEG**, **WebP**, or **TIFF** under **Format**, choose **Full**, **Long edge**, or **Percentage** under **Size**, then **Export…**.
Full resolution is the default. Long-edge pixels and integer percentages (1–100%)
reduce only, preserve aspect ratio to the nearest pixel, and show the output dimensions.
The save dialog captures the source, adjustments and export settings. PNG offers 8-bit or
16-bit sRGB with straight alpha (`.png`, default 16-bit). Eight-bit PNG includes optional
seeded RGB dithering; alpha uses nearest rounding without noise. JPEG produces 8-bit
sRGB (`.jpg`/`.jpeg`), with quality 1–100 (default 90), a selectable background (default white) for linear-light alpha
flattening, and optional seeded dithering that preserves solid 8-bit levels and clipped
black/white. WebP produces lossless 8-bit sRGB RGBA (`.webp`) with straight alpha and
optional seeded RGB dithering. Losslessness applies after color conversion and quantization;
decoded pixels match 8-bit PNG with the same settings, including hidden RGB at zero alpha.
TIFF produces uncompressed 8/16-bit sRGB RGBA (`.tif`/`.tiff`, default 16-bit),
with explicitly marked straight alpha and optional seeded RGB dithering at 8-bit.
Sixteen-bit PNG/TIFF never dither. All formats embed an output sRGB ICC profile.
PNG/JPEG/WebP validate nonempty source EXIF,
preserving it with recognized orientation reset to `1`.
Malformed or oversized EXIF fails without replacing the destination; empty EXIF is omitted.
JPEG EXIF is limited to 65527 bytes. TIFF omits source EXIF and writes Orientation=1;
the format controls state that camera metadata is omitted. See [the export contract](docs/export.md) for metadata
limitations. Colors outside sRGB are clipped. Editing or opening another image can continue
during export; **Cancel export** stops the captured job. Failure or cancellation before
publication leaves an existing destination untouched. Cancellation is checked within
CPU conversion bands as well as rendering and encoding. WebP cancellation during internal
frame compression waits for that pass to finish before discarding the result.
New exports use normal creation
permissions; replacements preserve existing Unix permission bits.

Resizing averages rendered pixels in linear light with alpha weighting, after the
basic adjustments and before color conversion/quantization. Transparent hidden color
does not darken edges; footprints containing only transparent samples produce zero RGB. Full-size
exports preserve hidden RGB as before. Reduction streams row bands with at most 64 MiB
of additional resize rows; there is no full resized working-image allocation. Every
source tile is still rendered. Enlargement and output sharpening remain pending.

Export shares the viewer's GPU device and submits one tile at a time, with asynchronous readback and non-blocking polling on a dedicated worker. It never allocates a full-image GPU texture or readback buffer. Tiles fit granted texture/buffer/dispatch limits, and padded 256-byte staging rows are stripped on readback. PNG encoding streams row bands, targeting 32 MiB per band (or one scanline for exceptionally wide images), plus tile buffers and the immutable half-float source. JPEG uses an additional RGB8 CPU buffer (3 bytes/pixel, capped at 512 MiB) for its explicit encoder. WebP uses an additional RGBA8 CPU buffer (4 bytes/pixel, capped at 512 MiB), plus encoder-owned transformed pixels and compressed frame data; the cap does not bound total process memory. WebP dimensions must each be 1–16384 pixels. Use a release build for large photos.

TIFF streams the same GPU row bands into strips targeting 1 MiB (or one wider scanline),
with a fallibly allocated conversion buffer and no full-image CPU output buffer.
Its uncompressed pixel payload is capped at 512 MiB, excluding headers/profile/strip tables;
the current import pixel budget keeps app exports below that bound. TIFF metadata is read
before applying pixel decoding limits so small tagged images retain their ICC profiles.

Rust 1.95 or newer is required by the locked GUI dependencies; the workspace uses edition 2024. The lockfile pins compatible eframe/egui-wgpu 0.37 and wgpu 30. Linux needs a Vulkan driver and the X11 or Wayland runtime libraries. Native file dialogs use `xdg-desktop-portal` and an installed portal backend; command-line paths and drop remain available without it. `--software-gpu` requires Mesa lavapipe on Linux or a WARP adapter on Windows. `WGPU_BACKEND` can restrict selection to `vulkan`, `dx12` or `metal`; GL is rejected.

```sh
cargo fmt --all --check
cargo check --workspace --locked
cargo test --workspace --locked
# Explicit integration test; fails if a software adapter is unavailable:
cargo test -p filter-gpu --tests --locked -- --ignored
# Basic-control and resizing tests for all export formats; needs a software adapter:
cargo test -p filter-app --locked image_exports_render -- --ignored
# Headless app shell: whole-window egui frames with queued input; needs a software adapter:
cargo test -p filter-app --locked app::tests -- --ignored
# >36MP export regression; requires roughly 800 MiB RAM.
# Debug builds can take several minutes:
cargo test -p filter-app --locked large_image_exports -- --ignored --nocapture
```

[The CI workflow](.github/workflows/ci.yml) runs on pull requests, pushes to `main`,
and manual dispatch. Its Ubuntu 24.04 and Windows 2025 jobs install stable Rust,
check formatting and Clippy, and run the locked workspace tests. Both jobs explicitly
run all ignored software-GPU tests serially, including the 36.6MP export regression;
a missing adapter fails the job. Ubuntu installs Mesa lavapipe and selects its Vulkan
ICD; Windows selects DX12 and requests the built-in WARP fallback. Test logs print
adapter details. Tests honor `WGPU_BACKEND` and continue to reject GL. Dependency
caches are saved only from `main`, and newer runs cancel superseded runs on the same ref.
The app-shell tests run whole-window egui frames against a software device with
synthetic pointer, scroll, drop and dialog-result input. These headless checks do
not exercise native window interaction, rendering to a surface or file dialogs.
Software-GPU viewport tests run before the separate large-image regression so
rendering failures surface without waiting for the 36.6MP exports.

The separate **SonarQube** job runs the workspace tests, including the ignored
software-GPU regressions on lavapipe, with `cargo-llvm-cov` and uploads LCOV
coverage and source analysis to SonarQube Cloud.
Fork and Dependabot pull requests still run coverage but skip the authenticated
analysis. Pushes to `main`, same-repository pull requests and manual runs publish
analysis using the repository's `SONAR_TOKEN` secret. Scanner packages use a weekly
cache saved only from `main`. The coverage report excludes only the slow
large-image export regression, which continues to run in the Linux/Windows matrix.
Unit-test modules live under each crate's `tests/unit` directory so coverage
excludes their bodies and SonarCloud can detect them as tests. CI passes an
absolute LCOV path because SonarCloud analyzes each crate as a separate module.
The scan step pins the SonarCloud URL in its arguments, overriding checked-out
configuration.

GPU tests check both sRGB viewport paths and alpha compositing, CPU/GPU parity across all basic controls, tiled-vs-untiled output, straight alpha, HDR/negative working values, and unaligned edge tiles. An additional app test checks PNG, JPEG, WebP and TIFF output against a captured basic-adjustment snapshot. The large export test processes a 6101 × 6001 image (36,612,101 pixels) with a granted 2048px texture limit and 256 MiB buffer limit, then checks every decoded 8/16-bit PNG/TIFF, lossless WebP and JPEG pixel (with a lossy tolerance for JPEG), plus a 257 × 253 streamed reduction. Both source dimensions exceed the texture limit, and a full-image readback would exceed the buffer limit. Ordinary workspace tests cover tile planning, output depth/profile/alpha, EXIF preservation and export/reload across all eight orientations for PNG/JPEG/WebP sources in both byte orders, PNG/TIFF bit depth, PNG/WebP/TIFF straight alpha and seeded dithering, JPEG quality and linear-light flattening, area-resize dimensions/color/alpha/band invariance, stream errors, and failure-safe file publication/cancellation. The app/export path polls without blocking. GPU parity and viewport test harnesses use bounded blocking polls for deterministic readback.

The region-preview software-GPU test compares cached tiles against untiled rendering
at native and fractional scales, including alpha, clipped regions and filtering seams
on both sRGB viewport paths. It also covers stale image uploads, adjustment changes,
cache reuse and eviction, persistent bindings across edits/panning, and injected tile
failures, retry, channel backpressure and worker disconnection. Ordinary tests cover the
100% refinement threshold, fractional tile coverage, memory bounds
and physical-pixel alignment at several display scales. Native Linux/Windows interaction
and refinement latency still require desktop validation.

Histogram GPU tests compare the CPU reference against neutral, exposure, mixed
adjustments, HDR/negative values, partial/zero alpha and invalid samples. They check
independent double-buffered jobs, saturation of both slots, recycled counter clearing,
source replacement and device/source limits. The app's headless UI test checks edit
coalescing, idle behavior and reserved-panel rendering; ordinary tests reject stale
image/edit results and cover retry, bins, luminance and clipping semantics.

Phase 1 adds concrete `EditState`, versioned preset JSON, selective section patches, transactional merges, unknown-node preservation, and desktop preset file save/load. The app uses `EditState` for all basic controls. A shared WGSL renderer drives preview and export, with a rayon CPU reference. Rendering of curves, detail, grading and custom nodes remains pending. See [the pointwise algorithms and API](docs/pointwise.md) and [the model and preset contract](docs/presets.md).

The workspace separates reusable image/color primitives (`filter-core`), device-sharing GPU rendering (`filter-gpu`), decoding and metadata (`filter-io`), and the desktop UI (`filter-app`). Neither the core, GPU, nor I/O crates depend on egui.

Remaining Phase 0 work: large rotated/wide-gamut image performance checks and Linux/Windows desktop validation. Perceptual gamut mapping, display dithering, other export formats/settings, a preset browser/thumbnails and additional filters are still pending. See [PLAN.md](PLAN.md) for the full roadmap.
