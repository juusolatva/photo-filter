# Photo Filter Studio

A native Rust photo editor for Linux and Windows, currently implementing [Phase 0](PLAN.md#phase-0-workspace-bootstrap-minimal-viewer--gpu-integration-spike).

Implemented: background JPEG/PNG/WebP/TIFF loading, EXIF orientation, embedded ICC conversion to straight linear Rec.2020 in half-float storage, alpha-aware 2048px proxies, and a GPU exposure viewport with fit, pan and pointer-centered zoom. Opening another image discards stale decode results. Images exceeding the initial per-image 2 GiB estimated working budget are rejected with an error. Input files are capped at 512 MiB, and retained ICC/EXIF metadata at 16 MiB each; decoder limits also apply. These are allocation guards, not a hard process memory limit. Tagged grayscale profiles are converted using their original single-channel layout.

```sh
cargo run -p filter-app -- path/to/photo.jpg
cargo run -p filter-app -- --software-gpu path/to/photo.png
```

Use **Open image**, **Ctrl+O**, or drag a file onto the window. Drag to pan, scroll to zoom, and select **Fit** to reset the view. Exposure ranges from −5 to +5 EV and leaves the source untouched. Zoom currently samples the proxy; true full-resolution regions arrive in Phase 2.

**Export PNG…** saves the full-resolution image at the exposure selected when the button was pressed. It produces a 16-bit sRGB PNG with straight alpha and an embedded sRGB ICC profile. Orientation is baked into the pixels; camera/EXIF metadata is omitted in this initial export. Colors outside sRGB are clipped, matching the initial viewport. Editing or opening another image can continue during export; **Cancel export** stops the captured job. Failure or cancellation before publication leaves an existing destination untouched.

Export shares the viewer's GPU device and submits one tile at a time, with asynchronous readback and non-blocking polling on a dedicated worker. It never allocates a full-image GPU texture or readback buffer. Tiles fit granted texture/buffer/dispatch limits, and padded 256-byte staging rows are stripped on readback. PNG encoding streams row bands, targeting 32 MiB per band (or one scanline for exceptionally wide images), plus tile buffers and the immutable half-float source. Use a release build for large photos.

A Rust toolchain supporting edition 2024 is required. The lockfile pins compatible eframe/egui-wgpu 0.37 and wgpu 30. Linux needs a Vulkan driver and the X11 or Wayland runtime libraries. Native file dialogs use `xdg-desktop-portal` and an installed portal backend; command-line paths and drop remain available without it. `--software-gpu` requires Mesa lavapipe on Linux or a WARP adapter on Windows. `WGPU_BACKEND` can restrict selection to `vulkan`, `dx12` or `metal`; GL is rejected.

```sh
cargo fmt --all --check
cargo check --workspace --locked
cargo test --workspace --locked
# Explicit integration test; fails if a software adapter is unavailable:
cargo test -p filter-gpu --tests --locked -- --ignored
# >36MP end-to-end export test; requires a software GPU and roughly 400 MiB RAM.
# Debug builds can take several minutes:
cargo test -p filter-app --locked large_png_export -- --ignored --nocapture
```

GPU tests check viewport sRGB output, tiled-vs-untiled exposure, CPU parity, straight alpha, HDR/negative working values, and unaligned edge tiles. The large export test processes a 6101 × 6001 image (36,612,101 pixels) with a granted 2048px texture limit and 256 MiB buffer limit, then checks every decoded PNG pixel. Both dimensions exceed the texture limit, and a full-image readback would exceed the buffer limit. Ordinary workspace tests cover tile planning, output depth/profile/alpha, stream errors, and failure-safe file publication. The app and tiled tests only poll without waiting; the original viewport test's blocking device poll is confined to that test.

The workspace separates reusable image/color primitives (`filter-core`), device-sharing GPU rendering (`filter-gpu`), decoding and metadata (`filter-io`), and the desktop UI (`filter-app`). Neither the core, GPU, nor I/O crates depend on egui.

Remaining Phase 0 work: large rotated/wide-gamut image performance checks and Linux/Windows desktop validation. Perceptual gamut mapping, display dithering, other export formats/settings and EXIF preservation, preset editing and additional filters are still pending. See [PLAN.md](PLAN.md) for the full roadmap.
