# Photo Filter Studio

A native Rust photo editor for Linux and Windows, with a Phase 0 viewer/export spike and the Phase 1 edit model and basic pointwise renderer from [the implementation plan](PLAN.md).

Implemented: background JPEG/PNG/WebP/TIFF loading, EXIF orientation, embedded ICC conversion to straight linear Rec.2020 in half-float storage, alpha-aware 2048px proxies, and a GPU basic-adjustment viewport with fit, pan and pointer-centered zoom. Opening another image discards stale decode results. Images exceeding the initial per-image 2 GiB estimated working budget are rejected with an error. Input files are capped at 512 MiB, and retained ICC/EXIF metadata at 16 MiB each; decoder limits also apply. These are allocation guards, not a hard process memory limit. Tagged grayscale profiles are converted using their original single-channel layout.

```sh
cargo run -p filter-app -- path/to/photo.jpg
cargo run -p filter-app -- --software-gpu path/to/photo.png
```

Use **Open image**, **Ctrl+O**, or drag a file onto the window. Drag to pan, scroll to zoom, and select **Fit** to reset the view. Exposure ranges from −5 to +5 EV. Relative temperature/tint, brightness, contrast, highlights/shadows, whites/blacks, vibrance and saturation range from −1 to +1. All edits leave the source untouched; **Reset adjustments** returns these controls to neutral while preserving other edit sections. Zoom currently samples the proxy; true full-resolution regions arrive in Phase 2.

Under **Presets**, use **Load preset…** (or drop a `.pfpreset.json` file), choose sections, and **Apply selected sections**. Unsupported active effects leave the current edit intact with an explanation; unknown future filters are preserved and skipped. **Save preset…** captures the current adjustments and selected sections as versioned JSON with a native overwrite dialog and atomic publication. Imports and serialized saves are limited to 16 MiB. See [the preset workflow and merge contract](docs/presets.md).

Choose **PNG** or **JPEG** under **Format**, then **Export…** to save the full-resolution
image with a snapshot of the current adjustments and export settings. PNG produces 16-bit
sRGB with straight alpha (`.png`). JPEG produces 8-bit sRGB (`.jpg`/`.jpeg`), with quality
1–100 (default 90), a selectable background (default white) for linear-light alpha
flattening, and optional seeded dithering that preserves solid 8-bit levels and clipped
black/white. Both embed an output sRGB ICC profile and validate nonempty source EXIF,
preserving it with recognized orientation reset to `1`.
Malformed or oversized EXIF fails without replacing the destination; empty EXIF is omitted.
JPEG EXIF is limited to 65527 bytes. See [the export contract](docs/export.md) for metadata
limitations. Colors outside sRGB are clipped. Editing or opening another image can continue
during export; **Cancel export** stops the captured job. Failure or cancellation before
publication leaves an existing destination untouched. JPEG cancellation is checked within
CPU conversion bands as well as rendering and encoding. New exports use normal creation
permissions; replacements preserve existing Unix permission bits.

Export shares the viewer's GPU device and submits one tile at a time, with asynchronous readback and non-blocking polling on a dedicated worker. It never allocates a full-image GPU texture or readback buffer. Tiles fit granted texture/buffer/dispatch limits, and padded 256-byte staging rows are stripped on readback. PNG encoding streams row bands, targeting 32 MiB per band (or one scanline for exceptionally wide images), plus tile buffers and the immutable half-float source. JPEG uses an additional RGB8 CPU buffer (3 bytes/pixel, capped at 512 MiB) for its explicit encoder. Use a release build for large photos.

A Rust toolchain supporting edition 2024 is required. The lockfile pins compatible eframe/egui-wgpu 0.37 and wgpu 30. Linux needs a Vulkan driver and the X11 or Wayland runtime libraries. Native file dialogs use `xdg-desktop-portal` and an installed portal backend; command-line paths and drop remain available without it. `--software-gpu` requires Mesa lavapipe on Linux or a WARP adapter on Windows. `WGPU_BACKEND` can restrict selection to `vulkan`, `dx12` or `metal`; GL is rejected.

```sh
cargo fmt --all --check
cargo check --workspace --locked
cargo test --workspace --locked
# Explicit integration test; fails if a software adapter is unavailable:
cargo test -p filter-gpu --tests --locked -- --ignored
# Basic-control PNG/JPEG export test; requires a software adapter:
cargo test -p filter-app --locked png_and_jpeg_export_render -- --ignored
# >36MP export regression; requires roughly 550 MiB RAM.
# Debug builds can take several minutes:
cargo test -p filter-app --locked large_png_and_jpeg_export -- --ignored --nocapture
```

GPU tests check both sRGB viewport paths and alpha compositing, CPU/GPU parity across all basic controls, tiled-vs-untiled output, straight alpha, HDR/negative working values, and unaligned edge tiles. An additional app test checks PNG and JPEG output against a captured basic-adjustment snapshot. The large export test processes a 6101 × 6001 image (36,612,101 pixels) with a granted 2048px texture limit and 256 MiB buffer limit, then checks every decoded PNG and JPEG pixel (with a lossy tolerance for JPEG). Both dimensions exceed the texture limit, and a full-image readback would exceed the buffer limit. Ordinary workspace tests cover tile planning, output depth/profile/alpha, EXIF preservation and export/reload across all eight orientations for PNG/JPEG/WebP sources in both byte orders, JPEG quality, linear-light flattening and seeded dithering, stream errors, and failure-safe file publication/cancellation. The app/export path polls without blocking. GPU parity and viewport test harnesses use bounded blocking polls for deterministic readback.

Phase 1 adds concrete `EditState`, versioned preset JSON, selective section patches, transactional merges, unknown-node preservation, and desktop preset file save/load. The app uses `EditState` for all basic controls. A shared WGSL renderer drives preview and export, with a rayon CPU reference. Rendering of curves, detail, grading and custom nodes remains pending. See [the pointwise algorithms and API](docs/pointwise.md) and [the model and preset contract](docs/presets.md).

The workspace separates reusable image/color primitives (`filter-core`), device-sharing GPU rendering (`filter-gpu`), decoding and metadata (`filter-io`), and the desktop UI (`filter-app`). Neither the core, GPU, nor I/O crates depend on egui.

Remaining Phase 0 work: large rotated/wide-gamut image performance checks and Linux/Windows desktop validation. Perceptual gamut mapping, display dithering, other export formats/settings, a preset browser/thumbnails and additional filters are still pending. See [PLAN.md](PLAN.md) for the full roadmap.
