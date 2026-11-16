# Photo Filter Studio

Native Rust/egui photo editor for Linux and Windows. Read `README.md` for implemented behavior and `PLAN.md` for the roadmap; planned features are not necessarily implemented.

## Architecture and rendering

- Keep `filter-core` independent of GUI and GPU APIs. It owns image storage, edit/preset models, color math and CPU references.
- Keep `filter-gpu` independent of egui. WGSL is the canonical renderer; use the shared basic-adjustment functions rather than duplicating filter algorithms.
- Keep file decoding, metadata and encoders in `filter-io`; desktop interaction and job scheduling belong in `filter-app`.
- Source pixels are straight-alpha, half-float linear Rec.2020. Sampled viewport textures are premultiplied. Spatial sampling must preserve alpha correctness and filtering borders.
- Display/export currently use sRGB clipping. Do not describe planned gamut compression, spatial filters or other unfinished features as implemented.
- The app shares eframe's GPU device and queue with background work. Never use blocking GPU polls or synchronous readback on the UI thread. Bound pending jobs and discard results from older images/edits.
- Only unmap readback buffers after a successful mapping. Retire and recreate buffers after mapping errors or disconnected callbacks; unmapping an idle buffer is a wgpu validation error. Check API assumptions against the versions in `Cargo.lock` and their dependency sources.
- Bound GPU allocations, readback and caches. Large images must not require full-resolution GPU textures or full-image readback buffers.
- Derive tile sampling from shared physical viewport coordinates. Windows/WARP exposed errors from hardware sampler precision and per-quad UV interpolation; preserve explicit float interpolation and strict pixel comparisons.
- Preserve export snapshots, cancellation and atomic destination publication. Failure before publication must leave existing files intact. See `docs/export.md`.

## Verification

- Use locked dependencies. Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings` and `cargo test --workspace --locked` for code changes.
- For GPU changes, run `cargo test --workspace --locked -- --ignored --skip large_image_exports --test-threads=1 --nocapture`. Tests require a software adapter and must fail rather than silently skip if it is unavailable.
- For export, resize or tile/readback changes, also run `cargo test -p filter-app --locked large_image_exports -- --ignored --test-threads=1 --nocapture`; the 36.6MP regression needs about 800 MiB RAM and several minutes in debug builds.
- CI runs Linux/Vulkan (lavapipe) and Windows/DX12 (WARP). Do not relax rendering tolerances to conceal adapter-specific failures; inspect coordinates, color transforms and alpha handling first.
- Headless checks do not establish native dialog behavior, desktop responsiveness or refinement latency. Report those limits accurately.
- Update the relevant docs and implementation status in `PLAN.md` when behavior changes.
