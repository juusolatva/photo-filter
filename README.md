# Photo Filter Studio

A native Rust photo editor for Linux and Windows, currently implementing the first slice of [Phase 0](PLAN.md#phase-0-workspace-bootstrap-minimal-viewer--gpu-integration-spike).

Implemented: background JPEG/PNG/WebP/TIFF loading, EXIF orientation, embedded ICC conversion to straight linear Rec.2020 in half-float storage, alpha-aware 2048px proxies, and a GPU exposure viewport with fit, pan and pointer-centered zoom. Opening another image discards stale decode results. Images exceeding the initial per-image 2 GiB estimated working budget are rejected with an error. Input files are capped at 512 MiB, and retained ICC/EXIF metadata at 16 MiB each; decoder limits also apply. These are allocation guards, not a hard process memory limit. Tagged grayscale profiles are converted using their original single-channel layout.

```sh
cargo run -p filter-app -- path/to/photo.jpg
cargo run -p filter-app -- --software-gpu path/to/photo.png
```

Use **Open image**, **Ctrl+O**, or drag a file onto the window. Drag to pan, scroll to zoom, and select **Fit** to reset the view. Exposure ranges from −5 to +5 EV and leaves the source untouched. Zoom currently samples the proxy; true full-resolution regions arrive in Phase 2.

A Rust toolchain supporting edition 2024 is required. The lockfile pins compatible eframe/egui-wgpu 0.37 and wgpu 30. Linux needs a Vulkan driver and the X11 or Wayland runtime libraries. Native file dialogs use `xdg-desktop-portal` and an installed portal backend; command-line paths and drop remain available without it. `--software-gpu` requires Mesa lavapipe on Linux or a WARP adapter on Windows. `WGPU_BACKEND` can restrict selection to `vulkan`, `dx12` or `metal`; GL is rejected.

```sh
cargo fmt --all --check
cargo check --workspace --locked
cargo test --workspace --locked
# Explicit integration test; fails if a software adapter is unavailable:
cargo test -p filter-gpu --test exposure --locked -- --ignored
```

The GPU test renders exposure into a small offscreen target and checks its sRGB pixels after a 256-byte-row-aligned asynchronous readback. Its blocking device poll is confined to the test; the app polls without waiting.

The workspace separates reusable image/color primitives (`filter-core`), device-sharing GPU rendering (`filter-gpu`), decoding and metadata (`filter-io`), and the desktop UI (`filter-app`). Neither the core, GPU, nor I/O crates depend on egui.

Remaining Phase 0 work: tiled full-resolution render/readback above 36MP, large rotated/wide-gamut image performance checks, and Linux/Windows desktop validation. The initial viewport clips display colors to sRGB; perceptual gamut mapping, display dithering, export, preset editing and additional filters are still pending. See [PLAN.md](PLAN.md) for the full roadmap.
