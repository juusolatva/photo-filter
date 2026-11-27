# Photo Filter & Preset Studio — Architecture & Implementation Plan

A cross-platform (Linux & Windows) native desktop photo editor focused on fast, non-destructive adjustments, detailed composable filters, and shareable presets—written in Rust with a modular workspace so core components can overlap with a procedural image generator and support camera RAW in the future.

---

## 1. Goals & Scope

### Core Features (v1)
* **Simple & Responsive Adjustments:** Exposure, contrast, brightness, highlights/shadows, whites/blacks, white balance (temperature/tint + eyedropper), vibrance, and saturation.
* **Detailed & Custom Filters:** Interactive tone curves (RGB + per-channel), 8-band HSL mixer, 3-way color grading wheels, sharpening/unsharp mask, clarity/local contrast, blur, vignette, procedural film grain, custom convolution kernels, 3D LUT import (`.cube`), and an extensible ordered filter stack with per-filter mix/blend modes.
* **Presets & Export:** Save/load full or partial filter pipelines as versioned presets, selective/partial preset application, live preset thumbnail previews, `.cube` 3D LUT export, and full-resolution or resized image export (long-edge / percentage resize with output sharpening; JPEG with quality, 8/16-bit PNG, lossless WebP—plus optional lossy WebP, TIFF) with safe overwrite confirmation.
* **Desktop UX:** Instant preview updates while dragging sliders, pan/zoom canvas with true full-resolution rendering at 100%+ zoom, split-screen before/after compare, live non-blocking RGB/luminance histogram, drag-batched undo/redo history, and native OS file dialogs on Linux and Windows.

### Non-Goals & Out of Scope
* **Camera RAW Decoding:** Deferred to a later phase, with a clean decoder boundary in `filter-io` and a wide-gamut working space (see §4.A) so RAW support drops in without changing the filter engine or UI.
* **HEIC / AVIF Decoding:** Not in v1. `image` has no HEIC decoder, and AVIF decoding requires the `avif-native` feature (dav1d C library). iPhones save HEIC by default, so revisit after v1.
* **Destructive Brush/Mask Painting & Layer Compositing:** Out of scope for v1 (though procedural radial/linear/luminance masks inside filter nodes fit cleanly).
* **Geometry Edits (Crop / Straighten / Perspective Warp):** Out of scope; only EXIF auto-orientation is applied on load so photos open upright. However, the edit state carries a `crop` rectangle from day one (always the full image in v1), and vignette plus radial/linear mask coordinates are defined relative to it, so adding crop later does not change the meaning of saved edits.

---

## 2. Technology Stack

| Layer | Choice | Why |
| :--- | :--- | :--- |
| **Language** | **Rust** (Edition 2024; virtual workspace manifest sets `resolver = "3"`) | Single fast native binaries on Linux & Windows; natural overlap with a Rust procedural image project. |
| **GUI Framework** | **`eframe` + `egui`** (`wgpu` backend, `persistence` feature) | Immediate-mode UI makes custom interactive widgets (tone curves, color wheels, histograms, split compare) straightforward and runs identically on Linux & Windows. |
| **GPU Acceleration** | **`wgpu`** | Runs natively on Vulkan (Linux) and DX12/Vulkan (Windows); the same code runs on software adapters (lavapipe / WARP) as the fallback and in CI. |
| **CPU Image I/O & Color Management** | **`image`** + **`tiff`** + **`moxcms`** (with `extended_range` feature) + **`rayon`** | `image` decodes/encodes JPEG, PNG (8/16-bit), TIFF, and lossless WebP, and embeds ICC (all four) and EXIF (JPEG/PNG/WebP); `tiff` directly streams uncompressed 8/16-bit output with an explicit unassociated-alpha tag; `moxcms` (pure Rust) transforms embedded ICC profiles into the working space; `rayon` powers pointwise CPU math, the LUT bake, and decode/encode work. *(Optional `webp` / `libwebp` feature for lossy WebP export.)* |
| **EXIF Editing** | **`image::metadata::Orientation::remove_from_exif_chunk`** | Resets recognized IFD0 orientation to `1` in a copy while retaining the remaining source EXIF bytes. General metadata rewriting remains deferred. |
| **Native File Dialogs** | **`rfd`** | Default features (`xdg-portal` + `wayland`, blocking via `pollster`) give native portal dialogs on Linux with no GTK headers; Win32 dialogs on Windows. |
| **Serialization, IDs & Paths** | **`serde`**, **`serde_json`**, **`uuid`**, **`directories`** | Versioned, human-readable preset files stored in standard OS config/data folders. |
| **GPU Interop Helpers** | **`bytemuck`**, **`half`** | Safe casting of uniform structs to WGSL shader buffers; `f16` source storage and `Rgba16Float` uploads. |

> **Important Dependency Note (`eframe` + `wgpu`):**  
> `eframe` pulls in `egui-wgpu`, which depends on a specific `wgpu` major version. Declare a semver-compatible `wgpu` in the root `[workspace.dependencies]` so `filter-gpu` and `filter-app` share identical `wgpu::Device`, `wgpu::Queue`, and `wgpu::TextureView` types without duplicate crates. **Do not** re-export `eframe::wgpu` from `filter-gpu`—keeping `filter-gpu` independent of `eframe` lets `filter-cli` and headless libraries use the GPU engine without compiling `egui`.

> **Important Device Note (`eframe` creates the device):**  
> `eframe` 0.37 creates the `wgpu` device with `Limits::default()` (only `max_texture_dimension_2d` raised to 8192) and **no optional features**, and its default instance also allows the GL backend, where the device gets WebGL2 downlevel limits (no compute shaders, 2048px textures). Therefore:
> * Override `WgpuSetupCreateNew::device_descriptor` in `NativeOptions` to request the adapter's real `max_texture_dimension_2d`, `max_buffer_size`, and `max_storage_buffer_binding_size`, plus `FLOAT32_FILTERABLE` when the adapter supports it. Checking `adapter.limits()` alone is not enough.
> * Restrict backends to `Backends::PRIMARY` (Vulkan/DX12/Metal) for the filter engine, or check `adapter.get_downlevel_capabilities()` and refuse compute paths on GL.

### Linux Build Prerequisites
Recent `winit` loads X11/Wayland/xkbcommon at runtime, so few or no `-dev` packages are needed. The Debian list commonly copied from egui's README (`libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev`) does not need `libssl-dev`. On Fedora, verify the minimal set with a clean Phase 0 build (e.g. `sudo dnf install libxkbcommon-devel` if the build asks for it) and record the result here.
* **`rfd` on Linux:** The default `xdg-portal` backend needs no GTK headers, but requires a running `xdg-desktop-portal` plus a backend (GNOME/KDE/GTK). Bare window managers (i3/sway without a portal) may have no dialogs, so drag-and-drop and command-line paths remain fallbacks. The optional `gtk3` feature needs `gtk3-devel` / `libgtk-3-dev` and `pkg-config`.
* Windows (`x86_64-pc-windows-msvc`) requires no extra external SDKs beyond standard MSVC build tools.

---

## 3. Workspace Architecture (Shared Core & Reusable Overlap)

Splitting the codebase into a Cargo workspace keeps the GUI, GPU shaders, I/O, and filter math cleanly decoupled. This directly benefits headless CLI workflows and procedural image generators:

```text
photo-filter/
├── Cargo.toml                        # Workspace manifest (resolver = "3", shared [workspace.dependencies])
├── PLAN.md
├── assets/
│   └── presets/                      # Built-in starter presets (.pfpreset.json, embedded via include_str!)
└── crates/
    ├── filter-core/                  # Headless pure-Rust crate (NO GUI or wgpu deps)
    │   ├── Cargo.toml
    │   └── src/
    │       ├── lib.rs
    │       ├── buffer.rs             # ImageBuffer (f16 or f32 RGBA, linear Rec.2020) + downsampling/tiling
    │       ├── color.rs              # Transfer functions, Rec.2020<->sRGB/P3 matrices, Oklab/OkLCh, gamut map, CAT16
    │       ├── edit.rs               # EditState: concrete, fully-defaulted parameters (what the engine renders)
    │       ├── pipeline.rs           # FilterNode, FilterOp, stage ordering, pixel-scale vs image-scale metadata
    │       ├── cpu_eval.rs           # rayon CPU implementation of pointwise stages (LUT bake, probes, tests)
    │       ├── histogram.rs          # RGB/Luma histogram & clipping stats
    │       ├── lut.rs                # .cube 3D LUT parser & generator/exporter
    │       └── preset.rs             # PresetPatch (Option fields), merge rules, migrations, embedded LUTs
    │
    ├── filter-gpu/                   # wgpu implementation — the canonical renderer for all stages
    │   ├── Cargo.toml
    │   └── src/
    │       ├── lib.rs
    │       ├── context.rs            # Borrows eframe's Device/Queue OR creates headless/software (fallback) adapter
    │       ├── shaders/              # WGSL shaders (color/tone, curves, mip-pyramid blur/bloom, grain, 3D LUT)
    │       └── executor.rs           # Ping-pong textures, uniform packing, region render, tiled export & readback
    │
    ├── filter-io/                    # Decoding, encoding, ICC color management, EXIF, future RAW hook
    │   ├── Cargo.toml
    │   └── src/
    │       ├── lib.rs
    │       ├── decoder.rs            # ImageDecoder trait -> DecodedImage (linear Rec.2020 + ICC/EXIF metadata)
    │       ├── standard.rs           # JPEG/PNG/WebP/TIFF via `image` + ICC transform via `moxcms`
    │       ├── encoder.rs            # Explicit format encoders, resize, dithering, alpha flatten, EXIF Orientation=1
    │       └── raw.rs                # (Future Phase) RawDecoder behind `feature = "raw"`
    │
    ├── filter-app/                   # Desktop GUI binary (eframe + egui)
    │   ├── Cargo.toml
    │   └── src/
    │       ├── main.rs               # eframe entry, icon, persistence, native options + wgpu device descriptor
    │       ├── app.rs                # App state, background job system (generations/cancel), undo/redo
    │       ├── preview.rs            # Proxy + full-res region render, CallbackTrait bridge, non-blocking readback
    │       ├── history.rs            # Drag-batched snapshot undo-redo (LUT payloads shared via Arc)
    │       └── ui/
    │           ├── top_bar.rs        # File ops, zoom, before/after toggle, clipping warnings, export dialog
    │           ├── viewport.rs       # Pan/zoom canvas, CallbackTrait split wipe, checkerboard alpha, eyedropper
    │           ├── inspector.rs      # Right panel: Basic, Curves, HSL, Wheels, Detail, Custom Stack
    │           ├── curve_widget.rs   # Interactive monotone cubic spline control-point editor
    │           ├── wheel_widget.rs   # 2D hue/saturation color grading wheel widget
    │           ├── histogram_ui.rs   # Custom egui Painter histogram
    │           └── preset_panel.rs   # Left panel: preset library, search, selective apply, async thumbnails
    │
    └── filter-cli/                   # (Phase 5) Headless batch CLI using filter-core / filter-gpu / filter-io
        ├── Cargo.toml
        └── src/
            └── main.rs               # Apply presets to folders of images from the terminal
```

### Overlap with a Procedural Image Generator
1. **`filter-core` + `filter-gpu` as a Post-Processing Library:** A procedural generator producing raw pixel buffers can depend on `filter-gpu` headlessly (software adapter if no GPU) to apply color grading, film grain, bloom, tone curves, or `.cube` LUTs without compiling `egui` or `eframe`. Pointwise-only grading is also available on pure CPU via `filter-core`.
2. **Shared Viewport Widget Potential:** `viewport.rs` (pan/zoom, split-screen compare, checkerboard alpha, cursor probe) depends only on `egui` + texture/callback handles, so it can be extracted into a shared `egui-image-viewport` crate later.

---

## 4. Color Space & Rendering Pipeline

Getting the math, alpha discipline, and GPU/CPU boundaries right early prevents banding, NaN artifacts, dark transparent fringes, and mismatched exports:

### A. Internal Precision, Color Management, & Alpha Discipline
1. **Working Space: Linear Rec.2020.**
   * Internal working space is **linear Rec.2020 primaries, D65** (not linear sRGB). Wide-gamut sources (Display P3, Adobe RGB) and future camera RAW data fit without large negative components, so HSL/saturation/grading behave predictably. The working space is fixed by `process_version` (see §6); changing it later would change how every saved preset looks.
   * Perceptual ops use Oklab/OkLCh computed from the working space (via XYZ/LMS), and display/export converts to the output space at the end.
2. **Input Normalization & ICC Color Management:**
   * Open files with `image::ImageReader::into_decoder()` so `icc_profile()`, `exif_metadata()`, and `orientation()` can be read before decoding; then construct `DynamicImage::from_decoder()` and apply `DynamicImage::apply_orientation` so pixels are upright in memory. (Note: in `image` 0.25+, `ImageReader::decode()` auto-orients by default—never combine `ImageReader::decode()` with a manual `apply_orientation`, or images will double-rotate.)
   * Transform with `moxcms` into a constructed linear-Rec.2020 destination profile (moxcms has `new_bt2020()`; build the linear-TRC variant). **moxcms clamps float transforms to `[0, 1]` by default**: enable the `extended_range` cargo feature and set `TransformOptions::allow_extended_range_rgb_xyz = true` (matrix-shaper profiles only). LUT-based (A2B/B2A) profiles still clamp—acceptable, but log it. Untagged images are treated as sRGB.
3. **HDR Headroom & Safe Math:**
   * Keep intermediate working values unclamped (`f32`, allowing values `> 1.0` or `< 0.0` transiently during exposure, sharpening, or matrix ops).
   * **Signed Cube Root in Oklab:** In WGSL, implement cube root as `sign(x) * pow(abs(x), 1.0 / 3.0)` (raw `pow` is undefined for `x < 0.0`). In Rust, `f32::cbrt` already handles negative inputs.
   * Apply soft gamut mapping / compression (OkLCh chroma compression toward the *output* gamut boundary) before final display/export clamping to avoid hue skews on oversaturated blues/magentas.
4. **Space-Aware Operations & Transfer Boundaries:**
   * **Linear RGB:** Exposure (`rgb * exp2(ev)`), relative White Balance (CAT16 chromatic adaptation from D65), Channel Mixer / Monochrome (3x3 matrix), Noise Reduction, Gaussian Blur, Unsharp Mask / Sharpening, Vignette, Film Grain, Bloom/Halation.
   * **Perceptual / Oklab / OkLCh:** Brightness (perceptual midtone lightness/gamma lift preserving black and white points), Contrast S-curve, Highlights/Shadows/Whites/Blacks, Tone Curves, Vibrance, Saturation, 8-band HSL (OkLCh hue), and 3-way Color Grading.
   * **Display-Referred Channel Ops & Tone Curve Domain:** Master Tone Curve evaluates in a perceptual encoding (`linear -> sRGB-style transfer (mirrored for negatives) -> 1024-entry 1D LUT -> linear`), using linear slope extrapolation above `1.0` so control points feel evenly distributed while preserving HDR headroom. Channel-dependent display-referred ops (per-channel R/G/B Tone Curves, `Levels`, `CustomCurve`, and `ColorMatrix3x3`) convert `working (Rec.2020) -> sRGB primaries + mirrored/extrapolated sRGB transfer -> op -> back to working` so R/G/B controls behave intuitively while preserving wide-gamut and HDR headroom.
   * **`.cube` 3D LUT Domain:** Standard creative `.cube` LUTs are sRGB/display-referred. Wrap evaluation in `working -> sRGB primaries + sRGB transfer (mapped by DOMAIN_MIN..DOMAIN_MAX) -> 3D LUT -> back to working`.
   * **Blend Modes:** Normal/Multiply/Screen/Overlay/Soft Light are defined for `[0, 1]` display-referred values. Custom-stack non-linear blending (`Screen`, `Overlay`, `Soft Light`) converts `working -> sRGB primaries + sRGB transfer` clamped to `[0, 1]` before blending and converts back afterward (`Normal` and `Multiply` may optionally blend in linear working space); this is specified per mode, not left to the node.
5. **Alpha Handling (Premultiplied vs. Straight):**
   * Store **straight (unmultiplied)** alpha in `DecodedImage` and final file export buffers (`PNG`, `WebP`, `TIFF`).
   * Around any spatial filter (`Blur`, `Sharpen`, `Convolution`, `Bloom`, `NR`), premultiply RGB by alpha (or weight taps by alpha) and unpremultiply afterward so transparent `(0, 0, 0, 0)` neighbors never bleed dark fringes into opaque edges.
   * `egui` / `egui_wgpu` expects **premultiplied alpha** in gamma-space `Rgba8Unorm` textures; the viewport pass composites over the checkerboard itself.
   * **JPEG export** (no alpha) flattens against a user-selectable background color (default white).
6. **Output Transform & Export Metadata:**
   * Convert working -> output sRGB (or Display P3 / target ICC via `moxcms`) right before display or file export, and embed the output ICC profile (`ImageEncoder::set_icc_profile`; supported for JPEG, PNG, WebP, TIFF).
   * Apply triangular dithering when quantizing to 8-bit. For export use seeded noise; for the display texture use a fixed (blue-noise or ordered) pattern so it doesn't shimmer frame to frame. Skip dithering for 16-bit PNG/TIFF, and allow disabling it in deterministic tests.
   * Use explicit encoders (`JpegEncoder::new_with_quality`, `PngEncoder` 8/16-bit, `tiff::encoder::TiffEncoder` (bounded strips and explicit unassociated alpha), `WebPEncoder::new_lossless`) rather than `save_with_format`.
   * **EXIF:** Copy `exif_bytes` and reset recognized IFD0 `Orientation` to `1` with `image::metadata::Orientation::remove_from_exif_chunk`. The streaming PNG encoder embeds these bytes through `png::Info::exif_metadata`; JPEG uses `ImageEncoder::set_exif_metadata` with a 65527-byte raw EXIF limit; lossless WebP uses `ImageEncoder::set_exif_metadata` with a 16 MiB raw EXIF limit. TIFF v1 deliberately omits source EXIF and writes only output ICC and Orientation=1 alongside image/alpha tags; the export controls state that camera metadata is omitted.
7. **Memory Budget:**
   * `Vec<[f32; 4]>` is 16 B/px (800 MB for a 50MP source). Store the immutable full-res source as **`f16` RGBA (8 B/px)** in `filter-core::buffer`, converting to `f32` per tile on the CPU paths. Set a soft memory cap from the start (not Phase 5) and warn before opening images that exceed it.

### B. Preview Proxy, Full-Res Region Rendering, `egui_wgpu` Viewport, & Tiled Export
* **Async Decode & Immutable Full-Res Source:** Decode on a background worker, keep a single immutable `Arc<DecodedImage>`, and immediately generate a downscaled preview proxy (area-filtered in linear light, longest edge `2048px`). Upload the proxy once as `Rgba16Float` (or `Rgba32Float` when `FLOAT32_FILTERABLE` was requested and granted).
* **Two Render Paths — Proxy and Full-Res Region:** The proxy cannot show pixel-scale effects: a 1px sharpen at 24MP is about 0.35px at 2048px, so sharpening, grain, and NR can't be judged on it. Therefore:
  * **Fit / zoomed-out:** render the whole proxy.
  * **Zoom ≥ proxy scale (e.g. 100%):** render the visible region from the full-res source, in cached tiles that are invalidated on parameter change. Show the upscaled proxy while region tiles render (progressive refinement), then swap in the full-res tiles.
  * Both paths use the same `filter-gpu` executor with a global-UV offset/scale, so they are the same code as tiled export.
* **Pixel-Scale vs Image-Scale Operations:** Every stage/op declares its scale in `pipeline.rs`:
  * **Image-scale** (defined in normalized units of the image diagonal; preview matches export): blur, bloom/halation, clarity radius, vignette, gradient masks, NR strength, and film grain in its default frame-relative mode (see [Film Emulation Looks](#film-emulation-looks)).
  * **Pixel-scale** (defined in *output* pixels; exact only in the full-res path, and the proxy shows a band-limited approximation): sharpening radius, film grain size in its optional output-pixel mode, convolution kernels (3×3/5×5), posterize/ordered dither patterns. Grain hashes integer *output-pixel* coordinates; at proxy scale it renders the statistically equivalent downsampled grain (reduced amplitude) instead of hashing proxy pixels.
  * When export resizes, pixel-scale ops run at the output resolution, and an optional **output sharpening** step (screen/matte/glossy amount) runs last.
* **WebGPU-Safe Ping-Pong Textures:** Use `Rgba16Float` intermediates. Structure compute passes to read from sampled `texture_2d<f32>` and write to `texture_storage_2d<rgba16float, write>`, or use fullscreen fragment passes (`RENDER_ATTACHMENT`).
* **Viewport Rendering (`egui_wgpu::CallbackTrait`) — decided:**
  * The canvas renders through an `egui_wgpu::CallbackTrait` paint callback. One pass handles zoom sampling (linear below 100%, `Nearest` above), checkerboard alpha, Before/After split wipe, clipping overlays, and output transform + display dithering.
  * If a plain image widget is ever needed (thumbnails, CPU-only buffers), `register_native_texture` requires `Rgba8Unorm`. Free textures with `renderer.free_texture`, or reuse an ID via `update_egui_texture_from_wgpu_texture` when dimensions change, to avoid VRAM leaks.
* **Non-Blocking Histogram & Cursor Readout:**
  * **Decided:** a GPU compute pass with workgroup-local `256 x 4` histograms merged into a global `u32` buffer, read back through double-buffered `map_async` staging buffers (1-frame latency). Both the 256-bin histogram and highlight/shadow clipping warnings are evaluated in the **target output color space** (gamma-encoded sRGB / Display P3 *after* soft gamut compression and output transform, *before* 8-bit dithering). The cursor probe reads a small patch through the same path.
  * Never call a blocking poll on the UI thread. `map_async` callbacks only fire when the device is polled, so call a **non-blocking** `device.poll` each frame and `ctx.request_repaint()` while any readback is pending.
* **GPU Sharing with Background Work:** Exports and thumbnails use **eframe's device** (`Device`/`Queue` are `Send + Sync`) from worker threads instead of creating a second device, which would double VRAM use. Long GPU work stalls UI frames because GPUs don't preempt, so submit **one tile per `queue.submit`** and yield between tiles. Headless devices are only for `filter-cli` and tests.
* **Tiled Full-Res Export & Readback:**
  * Tile when the image exceeds the granted `max_texture_dimension_2d` **or** when a readback would exceed `max_buffer_size` (a 36MP `Rgba16Float` readback is larger than the 256 MiB default). In practice, always read back per tile.
  * **`copy_texture_to_buffer` Alignment:** `wgpu::COPY_BYTES_PER_ROW_ALIGNMENT` requires `bytes_per_row` to be a multiple of `256` bytes (`32` pixels for `Rgba16Float`). Pad staging buffer `bytes_per_row` up to the next multiple of `256` on right-edge tiles and small probe readbacks, and strip row padding when unpacking on CPU.
  * Pass global image UV offset/scale uniforms into tile shaders so vignette, gradient masks, and film grain stay seamless across tile borders.
  * **Aprons & Multi-Stage Mip Pyramids:** Each mip-pyramid spatial stage (`Clarity`, `GaussianBlur`, `Bloom`) must build its pyramid from the intermediate entering *that* stage, not the raw input. Both 100%+ region rendering and tiled export obtain coarse pyramid levels (`mip >= k`) from the whole-image `≤2048px` proxy intermediate at that stage, while each full-res tile computes only fine pyramid levels (`mip < k`) with aprons sized from the sum of the fine-level reach of chained spatial stages (including small geometric aprons such as Lateral CA).
* **Background Job System:** Decode, region tiles, histogram, thumbnails, and export are jobs tagged with an edit **generation counter**. New edits cancel or discard stale jobs (latest wins); slider drags coalesce to at most one pending proxy render. Thumbnail and export work run on a dedicated low-priority thread pool, not the global `rayon` pool.

---

## 5. Filter Pipeline Specification

Design the pipeline in two complementary layers so simple edits stay fast and intuitive while detailed edits remain limitless:

### Layer 1: Standard Ordered Pipeline (Fixed Fast-Path Stages)
1. **Lens & Cleanup (linear):** Lateral Chromatic Aberration correction (per-channel radial scale relative to the **uncropped full-image center**—a lens property, not `crop`; geometric, so it runs first and contributes a small spatial apron in tiled/region renders), Noise Reduction (fast bilateral / edge-preserving smooth, before tone ops amplify noise).
2. **Input / Calibration (linear):** Exposure (`-5.0..+5.0` EV), White Balance (relative Temperature/Tint shift from D65 via CAT16), Channel Mixer / Monochrome (a 3x3 matrix in linear sRGB primaries with coefficients in `-2..+2`, folded in f64 into the white-balance matrix so it is one linear 3x3 on the GPU; monochrome outputs the luminance of the mixed color; the `color` preset section still stores it). Film spectral sensitivity is a linear-light weighting, so tone and curves then act on the converted image.
3. **Tone (perceptual):** Brightness (midtone lightness/gamma lift preserving black and white points, distinct from linear EV Exposure), Contrast, Highlights, Shadows, Whites, Blacks. **v1 decision:** Highlights/Shadows are pointwise (curve-based), so this stage stays LUT-bakeable; a locally adaptive version can come later under a new `process_version`.
4. **Local Contrast (spatial):** Clarity (pyramid-based).
5. **Tone Curves (perceptual):** Master curve + R, G, B curves (monotone cubic Hermite spline baked to a 1024-entry 1D LUT with linear extrapolation `> 1.0`; R/G/B evaluate in sRGB primaries).
6. **Color (perceptual):** 8-band HSL (Red, Orange, Yellow, Green, Aqua, Blue, Purple, Magenta; OkLCh hue), Vibrance, Saturation, 3-way Color Grading (Shadows/Midtones/Highlights color + balance in Oklab/OkLCh), Defringe (purple/green fringe desaturation).
7. **Detail (linear):** Sharpening / Unsharp Mask (amount, radius, detail threshold) — pixel-scale.
8. **Finishing:** 3D LUT overlay (`.cube` wrapped in sRGB + strength), Vignette (amount, midpoint, roundness, feather; relative to the crop rect), Procedural Film Grain (amount, size, roughness, plus the film-oriented tone response, color and scale mode in [Film Emulation Looks](#film-emulation-looks)—driven by a deterministic integer spatial hash).
9. **Output (export only):** Resize, output sharpening, gamut compression, transfer/ICC, dither, quantize.

Because Clarity (4) sits between pointwise stages, `.cube` export bakes pointwise Layer 1 stages (2, 3, 5, 6, and Finishing LUT) together with pointwise Layer 2 `FilterNode`s in stage order—skipping Clarity and any spatial stages/nodes—and applies soft gamut compression to sRGB; the export dialog lists any active skipped stages (see §6).

### Layer 2: Extensible Custom Filter Stack (`Vec<FilterNode>`)
For users who want to go beyond standard sliders and craft complex/artistic looks, add a user-editable stack of composable nodes:
* **Node wrapper:** `{ id, name, enabled: bool, position: StackPosition, blend_mode: BlendMode, opacity: f32, op: FilterOp }`
* **Insertion points (`StackPosition`):** `AfterCalibration` (linear, after stage 2: exposure, white balance and the channel mixer), `AfterColor` (after stage 6), `AfterFinishing` (after stage 8). Nodes run in list order within each position.
* **Supported `FilterOp` variants** (scale class in brackets):
  * `Levels { in_black, gamma, in_white, out_black, out_white }` [pointwise]
  * `CustomCurve { channel, points: Vec<[f32; 2]> }` [pointwise]
  * `ColorMatrix3x3 { matrix: [[f32; 3]; 3], bias: [f32; 3] }` [pointwise]
  * `GaussianBlur { radius: f32 }` [image-scale] (mip-pyramid blur with shared coarse proxy levels)
  * `ConvolutionKernel { size: 3 | 5, weights: Vec<f32>, normalize: bool }` [pixel-scale] (custom emboss, edge enhance, high-pass, custom blur)
  * `Duotone / GradientMap { shadows_color, highlights_color }` [pointwise]
  * `SelectiveColor / HueRangeMask` [pointwise]
  * `RadialOrLinearGradientFilter { geometry, feather, exposure_delta, tint, saturation }` [image-scale; geometry relative to crop rect]
  * `Bloom / Halation { threshold, radius, intensity }` [image-scale] (halation adds a tint; see [Film Emulation Looks](#film-emulation-looks))
  * `Posterize / OrderedDither` [pointwise / pixel-scale] (great overlap with procedural art)
  * `FilmGrain { amount, size, roughness }` [image-scale by default, pixel-scale option] (available as a stackable node as well as in Finishing; the current schema marks it pixel-scale and gains the fields in [Film Emulation Looks](#film-emulation-looks) when grain is implemented)
  * `Lut3D { lut: LutSource, strength }` [pointwise] (built-in ID, content-hashed library LUT, or inline embedded LUT payload)
  * `Unknown(serde_json::Value)` — preserves nodes from newer versions so they round-trip; rendered as a disabled pass-through with a warning badge.

### Film Emulation Looks
A long-term product goal is to build filters that emulate types of film (color negative, slide, black-and-white stocks) and save them as presets. Each trait of a film look maps to existing stages: tone response to tone curves; per-channel response to R/G/B curves; color character to HSL, color grading and the channel mixer; a complete measured or hand-made look to a `.cube` LUT with strength; black-and-white stocks to the channel mixer/monochrome; texture to grain, halation, vignette and softness. Design points for those stages:

* **Frame-relative grain.** Film grain belongs to the negative, not to the output file. By default, size grain in units of the image diagonal (a nominal film format, e.g. 35mm or 120, sets a sensible default), so a look keeps the same character at any export size and at Fit zoom. Keep an output-pixel mode for digital-style noise. Grain strength follows a luminance response that peaks in the midtones and falls off toward clipped black and white, with optional per-channel (color) grain for color stocks and monochrome grain for B&W. Grain stays deterministic (integer spatial hash on full-resolution source coordinates) so tiles, preview regions and exports agree. The existing `FilmGrain` schema (pixel-scale, amount/size/roughness) has no renderer yet; revise it when grain is implemented rather than shipping the output-pixel default first.
* **Halation as its own effect.** Halation is light scattering back through the film base around bright highlights, giving a red-orange halo. Model it as a thresholded highlight extraction, a frame-relative blur from the shared mip pyramid, and a tinted additive blend in linear light (tint default red-orange, adjustable), rather than generic white bloom. Threshold acts on scene-referred linear values so HDR highlights halate more.
* **Looks as an amount-controlled layer.** A film look must not overwrite the user's exposure, white balance or other basic corrections. A look is a named group of Layer 2 nodes (curves, color matrix, LUT, grain, halation) at the appropriate insertion points, with one **Amount** (0–100%) that scales the group's opacity, or each node's strength where opacity would be wrong (for example grain amount). Presets can save a look alone (custom stack only) or together with basic settings via the existing selective sections. The look group and its amount are part of the preset schema, versioned like other nodes.
* **Black-and-white responses.** The channel mixer's monochrome mode needs spectral-response presets (panchromatic, orthochromatic with weak red, red-filter, green-filter) expressed as mixer weights in linear sRGB primaries, plus a toning step (split tone or duotone) after conversion.
* **Generic naming.** Starter looks and bundled LUTs use descriptive generic names (for example "Warm Portrait Negative", "Cool Slide", "Orthochromatic B&W"), not film brand or product names, and are made in-house rather than copied from commercial packs.
* **Later: looks from references.** Generate a LUT (or curve + matrix fit) from a color chart photographed on film and digitally, or from a matched pair of reference images. This is the most accurate route to matching a specific stock and builds on the `.cube` baker.

Recommended order: (1) HSL mixer, color grading and channel mixer (pointwise, fits the current renderer and parity tests); (2) `.cube` import with strength and embedding in presets; (3) the spatial executor, unblocking frame-relative grain, halation and vignette; (4) the custom stack with look groups and Amount, then the preset browser with starter looks.

---

## 6. Preset System & `.cube` Export

### Edit State vs. Preset Patch
* **`EditState`** (`filter-core/src/edit.rs`) is what the engine renders: every parameter is a concrete value with a default, plus `process_version`, `crop`, and the custom stack. No `Option`s reach the filters or uniform packing.
* **`PresetPatch`** (`filter-core/src/preset.rs`) is what presets store: sections and fields are `Option<...>` with `#[serde(default)]`. `None`/omitted means "leave untouched" and an explicit value (including `0.0`) means "set". Applying is `edit_state.apply(&patch, &sections)`, where `sections` is the selective-apply mask.
* **Merge rules (per section):**
  * **Scalar sections** (`calibration` [Exposure, White Balance], `tone` [Brightness, Contrast, Highlights, Shadows, Whites, Blacks, Clarity], `detail` [Lateral CA, Noise Reduction, Sharpening], `finishing` [3D LUT overlay, Vignette, Film Grain]): field-by-field overwrite of present fields.
  * **Curves (`curves`):** each channel curve present in the patch replaces that channel wholesale; absent channels are untouched.
  * **Color (`color`):** scalar fields (`vibrance`, `saturation`, `channel_mixer`, `defringe`) overwrite field-by-field; nested `hsl` and `grading` overwrite per-band / per-wheel for present entries.
  * **Custom stack (`custom_stack`):** the patch's `custom_stack_mode` (`Replace` default, or `Append`) decides; nodes get fresh `id`s on apply.

### Native Preset Format (`.pfpreset.json`)
```json
{
  "schema_version": 1,
  "process_version": 1,
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "name": "Warm Film Fade",
  "author": "user",
  "tags": ["film", "warm", "portrait"],
  "patch": {
    "calibration": { "exposure": 0.15 },
    "tone": { "contrast": 0.12 },
    "curves": { "master": [[0.0, 0.05], [0.25, 0.22], [0.75, 0.78], [1.0, 0.95]] },
    "color": null,
    "detail": null,
    "finishing": null,
    "custom_stack_mode": "Replace",
    "custom_stack": [
      {
        "id": "7f9c2b1e-3a4d-4e5f-8a6b-1c2d3e4f5a6b",
        "name": "Soft Grain",
        "enabled": true,
        "position": "AfterFinishing",
        "opacity": 0.4,
        "blend_mode": "Normal",
        "op": { "type": "FilmGrain", "amount": 0.25, "size": 1.2, "roughness": 0.5 }
      }
    ]
  }
}
```
* **Schema Versioning & Migration:** `schema_version` describes the file layout. Loading runs a chain of `migrate_vN_to_vN+1` functions on the raw `serde_json::Value` before typed deserialization. Unknown fields are ignored (serde default; never `deny_unknown_fields`), and unknown `FilterOp` types deserialize to `Unknown` instead of failing the whole preset.
* **Process Versioning:** `process_version` pins the *algorithms*: working space, contrast curve shape, highlight/shadow method, and so on. Changing any algorithm's output bumps the process version and keeps the old code path, so old presets and edits render as they did. Users can opt in to upgrading an edit to the latest process.
* **Self-Contained Sharing:** When sharing `.pfpreset.json` files that use a custom `.cube` LUT (`Lut3D`), embed the LUT inline as base64 of compressed `f16` lattice data (not JSON float arrays—a 65³ LUT would be several MB), capped at 65³.
* **Selective Save / Apply:** Checkboxes aligned 1-to-1 with `PresetPatch` sections (*Calibration (Exposure/WB)*, *Tone*, *Curves*, *Color*, *Detail*, *Finishing*, *Custom Stack*) so you can apply a color grade without overriding exposure or white balance. Unchecked sections are saved as `None`.
* **Preset Browser UX:** Store user presets in the OS app-data directory (`directories::ProjectDirs`), and allow drag-and-drop import/export of `.pfpreset.json` files or folders. **Thumbnails** show the preset applied *on top of the current edit* with the current selective-apply mask, rendered at `128px` from a small (~256px) proxy on the GPU in small batches. They re-render debounced (~300ms after the last edit) and are cancelled by generation counter.

### `.cube` 3D LUT Import & Export
* **Import:** Parse standard Adobe `.cube` (`LUT_3D_SIZE N`, `DOMAIN_MIN`, `DOMAIN_MAX`, `R` fastest -> `G` -> `B` order), upload as a `wgpu::TextureFormat::Rgba16Float` 3D texture with hardware trilinear sampling (half-texel inset `(0.5 + x * (N - 1)) / N`) or tetrahedral interpolation in WGSL, wrapped as in §4.A.4.
* **Export:** Sample the pointwise stages of the current `EditState`—Layer 1 pointwise stages (2, 3, 5, 6, Finishing LUT) plus Layer 2 pointwise `FilterNode`s (with their blend modes and opacities) evaluated in stage order (`AfterCalibration`, `AfterColor`, `AfterFinishing`)—across an `N×N×N` sRGB `[0, 1]` grid (`sRGB -> working -> pointwise stages -> soft OkLCh gamut compression toward sRGB -> sRGB`, `N = 33` or `65`) using `filter-core::cpu_eval`, and write a valid `.cube` file (`R` fastest). Show a non-blocking notice in the export dialog listing all active spatial stages/nodes that were skipped (Lateral CA, NR, clarity, sharpen, blur, vignette, grain, bloom, convolution, gradient masks).

---

## 7. GUI & Interaction Design (`egui` / `eframe`)

### Layout & Ergonomics
* **Top Toolbar:** `Open`, `Export`, `Undo / Redo`, `Reset All`, Zoom (`Fit`, `100%`, `%`), `Before / After` mode (`Hold Space` or `Split Slider`), `Clipping Warnings` toggle (highlight blown whites in red / crushed blacks in blue).
* **Left Panel (Collapsible):**
  * **Presets Tab:** Search bar, tag filter, grid/list of preset cards with live thumbnail previews of the active photo, `+ Save Preset`, `Import`, `Export`.
  * **History Tab:** Edit action log to jump back to any state.
* **Center Canvas:**
  * Smooth pan (middle-drag or `Space` + drag) & cursor-centered scroll-wheel zoom.
  * At 100%+ zoom, the full-res region path renders the visible area, with a subtle indicator while tiles refine.
  * Draggable vertical split bar for Before/After comparison + checkerboard backdrop for transparent images.
  * Drag-and-drop image files or `.pfpreset.json` / `.cube` files directly onto the window.
* **Right Inspector Panel:**
  * **Live Histogram:** Smooth RGB + Luminance area plot at the top (non-blocking updates in target output space), plus post-pipeline pixel RGB/Hex/Oklab readout under cursor. **White Balance eyedropper** samples a small (e.g. `5×5`) averaged patch directly from the pre-WB linear Rec.2020 buffer on the CPU (`Arc<DecodedImage>`, after Lateral CA if active) rather than the post-pipeline GPU output, solving directly for CAT16 `(temperature, tint)` so downstream grading stages never cause sample drift.
  * **Collapsible Adjustment Sections:** Basic, Tone Curves (interactive spline editor), HSL / Color Mixer, Color Grading Wheels, Detail & Finishing, and Custom Filter Stack (`+ Add Node`, position selector, drag/button reorder, mute/solo, opacity slider). Pixel-scale controls (sharpening, grain) show a hint when viewed below 100%.
  * **Slider Ergonomics & Undo Batching:** Double-click any slider or label to reset to default; hold `Shift` for fine precision; numeric entry on double-click. Edit changes mark the history dirty and settle into one undo entry once no pointer button or key is held (typed slider values apply on Enter/focus loss), never per-frame during `.changed()`. Retained snapshots are bounded by step count and estimated bytes; future LUT payloads should be shared via `Arc` so they do not exhaust that budget.
* **Export Dialog:** Format, quality/bit depth, output color space (sRGB / Display P3), resize (long edge px / percentage / none), output sharpening, JPEG background color, metadata (keep/strip EXIF), and the `.cube` spatial-stage notice when exporting a LUT.
* **Export Safety:** Confirm before overwriting an existing file or source image on export; optionally support per-photo `.pfedit.json` sidecar saving/reloading (`EditState` + `process_version`).

---

## 8. Future-Proofing for Camera RAW & Cross-Platform Reliability

1. **Decoder Trait Boundary (`filter-io`):**
   ```rust
   pub struct DecodedImage {
       pub width: u32,
       pub height: u32,
       /// Linear Rec.2020 (D65) working space, straight (unmultiplied) RGBA, f16 storage
       pub pixels: Vec<[half::f16; 4]>,
       /// Orientation already applied to `pixels`; kept for metadata rewriting
       pub source_orientation: Orientation,
       pub icc_profile: Option<Vec<u8>>,
       pub exif_bytes: Option<Vec<u8>>,
   }
   ```
   Standard formats go through `image::ImageReader::into_decoder()` + `moxcms`. Later, enabling a `raw` Cargo feature plugs in a `RawDecoder` that demosaics, applies the camera matrix + white balance, and emits the same linear Rec.2020 buffer. Because the working space is already wide-gamut, camera colors don't arrive as large negative values. **License check before Phase 6:** `rawler` is LGPL-2.1 and LibRaw is LGPL-2.1/CDDL; confirm compliance for distributing an MIT-licensed static Rust binary, or keep RAW support behind an optional feature/separate build.
2. **EXIF Orientation & Metadata Hygiene:** Apply EXIF orientation on load so portrait photos display upright immediately, and rewrite `Orientation` to `1` in `exif_bytes` on export.
3. **GPU Compatibility Safeguards:**
   * Request the adapter's real limits and needed features through eframe's `device_descriptor` (see §2). Clamp the preview proxy well below the granted `max_texture_dimension_2d`, and tile exports and readbacks automatically. (`max_texture_dimension_3d` defaults to 2048, far above any 65³ LUT, so it needs no check.)
   * Use `Rgba16Float` intermediates (sampled reads + write-only storage or `RENDER_ATTACHMENT` passes) for broad compatibility on Linux Mesa/NVIDIA/AMD Vulkan and Windows DX12, with premultiplied `Rgba8Unorm` for any egui-registered texture.
   * **Fallback for broken GPU drivers:** eframe draws its own UI with `wgpu`, so broken drivers break the UI too, not just the filters, and a CPU-only filter path alone doesn't help. Provide `--software-gpu` (sets `force_fallback_adapter`, i.e. lavapipe on Linux / WARP on Windows) so the whole app, filters included, runs on a software adapter. Also honor `WGPU_BACKEND` for manual backend selection.

---

## 9. Phased Implementation Roadmap

### Phase 0: Workspace Bootstrap, Minimal Viewer & GPU Integration Spike

Implementation status (2026-11-16): workspace/viewer and tiled GPU exposure/readback spike implemented. The four crates, aligned GPU dependencies, custom device configuration, asynchronous oriented/ICC decoding, half-float source storage, alpha-aware proxy, and callback exposure viewport are in place. Full-resolution 8/16-bit sRGB PNG export shares eframe's GPU device, uses one tile per submit with non-blocking map/readback, streams bounded row bands, and supports progress/cancellation and atomic destination replacement. Exposure WGSL is shared by preview and tiles. Software-GPU tests validate tiled/untiled/CPU parity and every output pixel of a 6101 × 6001 (36.6MP) PNG with a 2048px texture limit and 256 MiB buffer limit, including padded edge rows and alpha. The regression also checks every pixel of dithered 8-bit PNG and full-resolution JPEG with linear-light alpha flattening. Output ICC is embedded; PNG source EXIF preservation with orientation normalization is now implemented in Phase 1. Additional export settings remain open. Large rotated/wide-gamut image performance checks and Linux/Windows desktop validation remain open; Phase 0 is not yet complete.

* **Deliverables:**
  * Initialize Cargo workspace (`Cargo.toml` with `resolver = "3"` + `filter-core`, `filter-gpu`, `filter-io`, `filter-app`) with aligned `wgpu` version.
  * `eframe` desktop window launching cleanly on Linux and Windows, with a custom `device_descriptor` requesting adapter limits/features and `Backends::PRIMARY`.
  * Background-threaded decode (`rfd` or drag-and-drop) handling EXIF orientation + `moxcms` (`extended_range`) ICC conversion into `Arc<DecodedImage>` (linear Rec.2020, `f16`), generating a `2048px` proxy before GPU upload.
  * **GPU spike (de-risking):** one exposure shader in `filter-gpu`, drawn through a `CallbackTrait` viewport on eframe's device, plus a tiled full-res render + non-blocking `map_async` readback of a >36MP image (respecting `256`-byte `COPY_BYTES_PER_ROW_ALIGNMENT` row padding).
* **Done Criteria:** Can open a large wide-gamut/rotated JPEG or PNG on both Linux and Windows and pan/zoom smoothly at 60fps. Display P3 test colors survive the load without clipping. The exposure spike exports a >36MP image via tiled readback without exceeding device limits.

### Phase 1: `filter-core` Data Model, Pointwise Engine & Basic Export

Implementation status (2026-11-16): initial edit/preset model implemented in `filter-core`. Concrete calibration, tone, curves, color/HSL/grading, detail and finishing defaults, `process_version`, full-image `crop`, selective optional patches, transactional merge rules, stack append/replace with fresh UUIDs, schema-v1 JSON fixtures, validated edit deserialization, bounded preset imports/stacks, version rejection/migration boundary, and opaque unknown-operation round-trips are in place. The app stores its adjustments in `EditState`. Basic exposure, relative CAT16 temperature/tint, Oklab brightness/contrast/highlights/shadows/whites/blacks, vibrance and saturation now share canonical WGSL between proxy preview and tiled PNG/JPEG/WebP/TIFF export, with a matching rayon CPU reference and immutable export snapshots. Color math includes Rec.2020/XYZ/sRGB/P3 matrices and signed Oklab/OkLCh. Software-GPU tests cover f32/half-float parity, every basic control, negative/HDR/alpha inputs, tile invariance and viewport transfer paths. Levels, color matrix and grain have typed node schemas/scale metadata only. Other node schemas and finishing LUT payload/overlay contracts remain deferred to their engine work. Preset file dialogs and selective save/load are implemented with bounded imports, atomic saves, unknown-node preservation, transactional rejection of unsupported selected effects, and immutable save snapshots. PNG export now validates nonempty source EXIF with `kamadak-exif`, omits empty metadata, and resets recognized orientation to `1` without mutating the captured source. Malformed EXIF fails before PNG header writes and leaves existing destinations intact. Export/reload tests cover all eight orientations, both byte orders, and PNG/JPEG/WebP inputs. Other source EXIF descriptions are retained unchanged; see [docs/export.md](docs/export.md). JPEG export adds quality 1–100, selectable background alpha flattening in linear light, seeded 8-bit dithering, ICC/normalized EXIF, snapshot export settings and cancellation during CPU conversion (every 4096 pixels) and encoding. Dithering preserves representable solid/clipped levels. JPEG uses an additional RGB8 CPU buffer (3 bytes/pixel, capped at 512 MiB); GPU rendering remains tiled. PNG now offers 8/16-bit depth selection (default 16-bit) and optional seeded 8-bit RGB dithering with undithered straight alpha. Both PNG depths stream bounded bands and check cancellation every 4096 converted pixels. Existing 16-bit APIs remain available. Tests cover JPEG APP1 limits, metadata/color/alpha, quality/dithering, PNG tile/band invariance and >36MP output at both PNG depths and JPEG. Lossless WebP export now shares PNG8 straight RGBA quantization/dithering, embeds output ICC and normalized EXIF, and captures format/settings with the source snapshot. It uses an additional RGBA8 CPU buffer (4 bytes/pixel, capped at 512 MiB), plus internal encoder buffers; dimensions are limited to 16384 pixels per edge. Cancellation during internal frame compression waits for that pass to finish and then prevents publication. Tests cover exact decoded PNG8/WebP parity across seeds/bands/tiles, metadata, limits, write errors, cancellation and >36MP output. Uncompressed TIFF export now offers 8/16-bit depth (default 16), shared straight RGBA quantization and optional 8-bit RGB dithering, output ICC, explicit unassociated alpha and Orientation=1. Source EXIF is omitted with an in-app notice. The direct `tiff` encoder streams bounded strips targeting 1 MiB (or one wider scanline), with a fallible conversion buffer and a 512 MiB uncompressed pixel-payload cap; no full-image CPU output buffer is needed. TIFF metadata is read before applying pixel decoding limits on import so small images retain their ICC profiles. Tests cover exact PNG parity across seeds/bands/strips/tiles at both depths, orientation reloads, tags, invalid inputs, I/O failures, cancellation/publication safety, and every pixel of >36MP TIFF output. Export resizing now captures full size (default), long edge or integer 1–100% settings, with an output dimension preview and reduction-only aspect-preserving rounding. Rendered GPU row bands are area-filtered on the CPU in linear Rec.2020 with alpha weighting after pointwise adjustments and before output conversion, flattening and dithering. Integer global footprints preserve tile/band invariance. Two f64 accumulator/cache rows and one f16 output row use fallible allocation with a combined 64 MiB cap; no full resized working image is allocated. Identical dimensions bypass filtering and preserve existing hidden-color behavior. Cancellation, invalid streams and late render errors preserve destinations across all formats. Tests compare an independent area reference, all formats/depths and resize settings against captured adjustments across tile sizes, metadata/identity behavior, and 36.6MP-to-257px reduction. Enlargement and output sharpening remain pending. The preset browser/thumbnails, remaining export settings/optional lossy WebP, output gamut compression/display dithering, grading/detail and custom-node execution remain open (tone curves now render; see Phase 3); Phase 1 is not complete. See [docs/presets.md](docs/presets.md) and [docs/pointwise.md](docs/pointwise.md) for the current contracts.
* **Deliverables:**
  * Define `EditState` (concrete), `PresetPatch` (`Option`-wrapped) with merge rules, `process_version`, the `crop` rect, and color math (transfer functions, Rec.2020 matrices, Oklab with signed `cbrt`, CAT16) in `filter-core`.
  * Implement pointwise stages (Exposure, White Balance, Brightness, Contrast, Highlights/Shadows, Whites/Blacks, Vibrance, Saturation) **in WGSL as the canonical renderer**, plus the matching `rayon` CPU implementation in `cpu_eval` (used for LUT bake, probes, and parity tests).
  * Wire right-side sliders to the GPU proxy render, plus background export using explicit encoders (JPEG quality, 8/16-bit PNG, lossless WebP, 8/16-bit TIFF), resize, alpha flattening for JPEG, triangular 8-bit dithering, ICC embedding, and EXIF `Orientation = 1` rewriting.
  * Implement preset JSON save/load with schema migration scaffolding and `Unknown` node preservation.
* **Done Criteria:** Non-destructive edits work end-to-end; presets save/reload identically (including partial presets and unknown future nodes); exports preserve color accuracy and correct orientation; unit tests cover color conversions, merge rules, and preset schema round-trips.

### Phase 2: Real-Time Pipeline, Full-Res Region Rendering & Job System

Implementation status (2026-11-16): the basic pointwise viewport now progressively overlays full-resolution source tiles at 100% physical display scale and above. Minified views retain the area-filtered proxy to avoid aliasing; filtered intermediate-scale regions remain pending. A single background upload request at a time uses eframe's device/queue; a center-first LRU cache holds at most 63 tiles (512px interiors plus one-pixel bilinear borders), within a 128 MiB texture-payload budget excluding driver overhead and in-flight work. Larger visible source regions retain the proxy outside that central subset. Immutable source tiles survive edit changes and each draw uses the current validated basic-adjustment snapshot, avoiding rendered-edit cache invalidation and stale edits. Image-generation checks discard old uploads. Failed tiles are skipped while other tiles continue; Retry preview releases the cache and retries without decoding again. Full-channel backpressure and worker disconnection are handled separately. Per-tile and proxy uniform buffers/bind groups persist across repaints, with updates batched through a reusable staging belt. Sampling uses shared physical viewport bounds rather than per-quad interpolated UVs, and explicit f32 bilinear interpolation avoids adapter-dependent sampler weight precision at high-contrast tile boundaries. A compile-time assertion requires the cache budget to hold at least one bordered tile. The 100% control maps one source pixel to one physical display pixel with aligned image/callback bounds, including across resize/DPI changes. Software-GPU tests compare tiled and untiled regions at native/fractional scales on both sRGB target paths, including alpha/seams, stale uploads, edits, cache reuse/eviction, persistent bindings, injected tile failures, retry, channel backpressure and disconnection. Unit tests cover the 100% refinement threshold, clipped/fractional coverage, cache bounds and HiDPI alignment. The general multi-pass/spatial executor and native desktop/latency validation remain open; Phase 2 is not complete.
Histogram status (2026-11-16): live 256-bin RGB/encoded-luminance curves and per-channel/any-RGB clipping counts now measure the whole edited proxy, independent of pan/zoom. Canonical basic WGSL and shared viewport output-transform helpers operate before checkerboard/dithering; transparent pixels are omitted, partial alpha counts once, and active nonfinite samples are tracked separately. A 16×16 compute dispatch uses workgroup-local atomics and double-buffered 4160-byte asynchronous readback, with at most two jobs in flight and no full-image readback/extra source texture. Edit/image generations reject stale results and errors; latest edits coalesce, idle repaints submit no work, and dimmed older curves carry an Updating label with counts hidden. Retry handles current readback failures; map errors and disconnected callbacks retire their buffers without unmapping them, and the next submission allocates replacements. A fault-injection GPU regression verifies that both failure paths and retry produce no wgpu validation errors. Core, software-GPU and headless egui tests cover distributions/clipping/alpha, simultaneous snapshots, slot backpressure/reuse, source replacement, limits, stale generations and UI scheduling. Native performance and Windows/DX12 verification remain pending for this change. See [docs/histogram.md](docs/histogram.md).

Cursor probe status (2026-11-16): a readout below the histogram shows zero-based oriented-source coordinates, edited 8-bit sRGB RGB/hex, Oklab of the clipped output color and separate alpha. It measures the containing full-resolution source pixel at any zoom, rather than the proxy or interpolated screen color. One original straight-alpha pixel passes through canonical basic WGSL and the shared sRGB output transform; no full-image texture, tile upload or image readback is required. A single compute invocation and two 32-byte asynchronous readback slots bound GPU work; pointer/edit changes coalesce and image/edit/pixel generations reject stale results before display. Transparent/invalid pixels have explicit labels, idle repaints submit no work, and leaving the image clears values. Current mapping failures offer Retry color probe; failed/disconnected buffers are retired without unmapping and replaced on retry. Core, software-GPU and headless UI checks cover color/alpha/parity, coordinates/DPI, backpressure, idle behavior, image/edit/pointer invalidation and failure recovery. Native Linux/Windows interaction and responsiveness remain unverified. See [docs/probe.md](docs/probe.md).

* **Deliverables:**
  * Generalize the `filter-gpu` executor (sampled reads + write-only `Rgba16Float` ping-pong or fragment passes) with global-UV tiles shared by proxy, region, and export renders.
  * **Full-res region path** for zoom ≥ proxy scale, with tile caching and progressive refinement.
  * Background job system with generation counters, cancellation, and one-tile-per-submit export on eframe's device; non-blocking `device.poll` each frame.
  * GPU histogram (workgroup-local atomics + double-buffered readback) and cursor probe.
  * Headless test runner on a software adapter; **CPU-vs-GPU parity tests for pointwise stages** (dithering disabled).
* **Done Criteria:** Real-time 60fps slider response at 2048px preview resolution on both Linux (Vulkan) and Windows (DX12). 100% zoom shows true full-res pixels within ~200ms of the last edit. Exports don't drop UI frames below 30fps.

### Phase 3: Detailed Filters & Custom UI Widgets
* **Deliverables:**
  * Interactive **Tone Curve** widget (monotone cubic spline, add/move/remove points, Master/R/G/B tabs, perceptual-domain evaluation with HDR extrapolation).
  * Highlight/shadow clipping overlay + WB eyedropper.
  * **8-band HSL Mixer** and **3-way Color Grading Wheels** (Oklab/OkLCh + soft gamut mapping).
  * Spatial filters in WGSL (alpha-weighted): **Noise Reduction**, **Lateral CA**, **Sharpening (Unsharp Mask)**, **Clarity** (mip-pyramid), **Gaussian Blur** (mip-pyramid with shared coarse levels for tiling), **Vignette**, **Procedural Film Grain** (output-pixel hash, band-limited at proxy scale), and **output sharpening**.
  * **Before/After** split-wipe & toggle + drag-batched **Undo/Redo** command history.
Tone curve status (2026-11-27): master and R/G/B curves render in canonical WGSL and the CPU reference, between the tone and color stages. Points are interpolated by PCHIP monotone cubic Hermite splines, baked to 1024-sample tables per curve. Curves evaluate on mirrored sRGB-encoded values in sRGB primaries (master, then channel), with linear end-tangent extrapolation below 0 and above 1. Identity curves skip the stage, keeping exact neutral output. Control-point y is now limited to −1…2. The 16 KiB `ToneCurves` uniform is shared per viewport renderer and written per job for export tiles, histogram and probe. The side panel's **Tone curve** editor has RGB/R/G/B tabs; click adds a point, drag moves it, double-/right-click removes it, it shows an input → output readout and per-channel reset, and each gesture is one undo step. Tests:
- Core: spline shape preservation and tables.
- Software GPU: f32/f16 parity for five curve cases, viewport tile/untiled/native-pixel agreement, histogram, probe and all export formats.
- UI: editor unit tests, plus a headless app test driving drag, click, right-click, tabs, reset and undo.

Half-float tile parity now uses a one-f16-step bound instead of an 8-bit display comparison. Not implemented: a histogram behind the curve plot, numeric point entry and a precision modifier. Native desktop interaction remains unverified.

Before/after status (2026-11-27): the toolbar **Before/after** toggle splits the viewport at a draggable divider positioned as a fraction of the canvas width; holding Space shows the original across the canvas. The split is one physical framebuffer x in the renderer's shared viewport uniform (`ExposureRenderer::set_before_until`), so proxy and full-resolution tile draws switch on identical columns without extra frames, passes or textures. Left of it the fragment shader skips the basic adjustments and keeps sampling, alpha compositing and output encoding. The divider is drawn on the same physical boundary and registered above the canvas, so dragging it does not pan. Space presses are consumed (except in text fields) so they cannot click a keyboard-focused button. Histogram, probe and export are unchanged and keep describing the edit. Software-GPU tests check exact original/edited columns either side of a split for tiled and untiled frames, the full original and reset. Unit tests check pixel-boundary alignment at fractional display scales. A headless app test covers toggling, divider dragging without panning, hold/repeat/release of Space and a Tab-focused Reset button. Native desktop interaction remains unverified. A hold-to-compare toolbar control and comparing against a snapshot other than the original are not implemented.

Clipping overlay status (2026-11-27): the side panel's **Shadow clipping** and **Highlight clipping** checkboxes, and **J** (both on unless both already on; ignored while a text field has focus), flag clipped pixels in the viewport as opaque pure blue (shadows) and pure red (highlights; they win when both hold). A pixel is flagged when any channel of the unclipped linear-sRGB output would display as 8-bit 255 or 0: the histogram's any-RGB endpoint tests widened by half a display step, so f32 interpolation noise cannot flip results between tiles and the untiled path. The histogram keeps its exact endpoints; the helpers `shadow_clipped`/`highlight_clipped`/`any_nonfinite3` moved to `output.wgsl` with its results unchanged. Nonfinite output and fragments with alpha below 0.5/255 are not flagged. Flags ride in the renderer's shared viewport uniform (`ExposureRenderer::set_clipping_overlay`), so proxy and full-resolution tile draws agree with no extra pass or per-tile uniform. The overlay applies to whatever a fragment shows, including the original side of Before/after and the Space hold. It is view state only: not in `EditState`, undo history or presets, kept across image loads within the session; export, histogram and cursor probe are unaffected. Software-GPU tests check exact overlay colors for white, black, mid-gray, both-way, single-channel, transparent and half-transparent pixels with every flag combination on tiled and untiled frames on both target paths, byte-identical output with flags off, byte-identical tiled and untiled renders of a multi-tile image, a +3 EV edit pushing mid-gray into highlight clipping, and the before side of a split; headless app tests cover checkboxes, J, held-key repeats toggling once, text-field gating and persistence across loads, and a unit test the toggle rule. Native desktop interaction remains unverified. See [docs/histogram.md](docs/histogram.md#clipping-overlay).

WB eyedropper status (2026-11-27): the side panel's **White balance picker (W)** button (and **W**, ignored while a text field has focus) arms a picker; a primary click, not a drag, on an image pixel sets Temperature and Tint together as one undo step and disarms it. Esc, the button or W again cancels without a change; clicks on the letterbox, the Before/after divider or while Space is held do nothing and keep it armed, as do sampling errors, and loading another image cancels it. It is pure CPU with no GPU readback: exposure is a scalar that preserves chromaticity and precedes the relative CAT16 white balance, so the original straight-alpha linear Rec.2020 pixels carry exactly the chromaticity white balance sees, and the result is independent of the current Temperature, Tint and exposure and of later stages. `filter_core::white_balance` averages a 5 x 5 neighborhood (alpha-weighted, clipped at edges, skipping alpha <= 0 and nonfinite pixels) around the pixel that `source_pixel` maps from the click, finds the CAT16 target white whose adaptation makes that color neutral (diagonal in LMS, so `LMS_target ∝ LMS_D65² / LMS_sample`) and inverts the linear x/y control mapping, sharing the CAT16 matrices and control constants with `relative_white_balance`. Results are limited to [-1, 1] with a flag shown in the status line; transparent, too-dark (Y < 0.001) and invalid (nonfinite or non-positive cone response) samples return typed errors. Later per-channel stages such as RGB curves can reintroduce a cast. Core tests cover a (T, Q) round trip against the inverse white-balance matrix, resulting Oklab a/b near zero, neutral gray, clamping, rejected samples and neighborhood edge, alpha and skip rules; a headless app test covers button, click, drag, letterbox, divider, Space, transparent error, one-step undo/redo, Esc, the button, W, text-field gating and load cancellation. Native desktop interaction remains unverified. See [docs/pointwise.md](docs/pointwise.md#white-balance-picker).

Undo/redo status (2026-11-27): whole-`EditState` snapshot history is implemented in `filter-app` (`history.rs`). Slider drags, held keys and typed slider values (applied on Enter/focus loss) settle as one step once no pointer button or key is held; preset application and **Reset adjustments** are steps; unchanged results record nothing. Toolbar **Undo/Redo** and Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y act unless a text field has focus; undo during an unfinished gesture first records it. A new change discards redo, and opening an image starts fresh history. Retained undo+redo steps are capped at 100 and 64 MiB by `EditState::estimated_bytes` (curve points, node names and opaque payloads), evicting the oldest undo then furthest redo. Unit tests cover stepping, redo invalidation and both limits; a headless app test drives a real slider drag, preset apply, reset, buttons and shortcuts. Native desktop interaction remains unverified. Before/after compare and the remaining Phase 3 deliverables remain open.

HSL mixer status (2026-11-27): the eight-band OkLCh HSL mixer (red, orange, yellow, green, aqua, blue, purple, magenta) renders in canonical WGSL and the CPU reference, after the tone curves and before vibrance/saturation, in the same Oklab round trip (`color_stage`: HSL, then vibrance/saturation). Hue, saturation and lightness per band are -1..+1, validated, with zero neutral; any active band routes through the post-curve color stage while all-zero HSL, exposure-only and vibrance/saturation-only output stay bit-identical. The shader uses no `atan2/sin/cos`: band weights come from a rational pseudo-angle of Oklab (a, b), two adjacent bands blend with a smoothstep (partition of unity, flat at each center, continuous across the magenta/red seam), hue rotates by an exactly unit-norm Cayley half-angle (full scale 2*atan(tan 15 deg) = 30 degrees), saturation scales (a, b) by 0..2, and lightness scales L by `exp2(0.5 * value)` (about +-1.5 stops, black stays black). Everything is gated by `smoothstep(0.005, 0.04, chroma)`, so gray and muted colors below the gate are untouched; strongly opposed hue shifts in adjacent bands compress the hues between them. Trig and `tan` run once on the CPU when building uniforms, and constants live once in `filter-core::grading` (a test parses the WGSL literals). The `BasicParams` uniform grew to its final 16-row, 256-byte layout (`BASIC_ROWS`, rows 12-14 and flag lanes y/w for color grading), so tile uniforms are 272 bytes, probe and histogram uniforms 256 bytes and viewport frames 304 bytes. The side panel's collapsing **Color mixer (HSL)** has Hue/Saturation/Luminance/All tabs, swatch-labeled bands with gradient tracks, -100..+100 sliders, double-click zeroing and per-channel and global reset; each gesture is one undo step and the tab is app state. `EditState::reset_hsl` is added; `reset_basic` still preserves HSL. Channel mixer, monochrome and defringe remain rejected by the renderer. Tests: CPU band centers, constants, partition of unity, continuity, per-band exactness, global and gated behavior, finite extremes, validation and row layout; software-GPU f32/f16 parity for per-band, random-band and stacked HSL cases with hue sweeps across the chroma gate, near-black, negative and HDR pixels (tolerance unchanged), plus the HSL path in the viewport, histogram and probe tests; unit tests for the mixer UI helpers and a headless app test for a slider drag and resets as undo steps. The constants (gate, 30 degrees, 0.5 exponent) are analysis-based and should be tuned on real images before starter looks freeze them. Native desktop interaction and slider latency remain unverified. See [docs/pointwise.md](docs/pointwise.md#hsl-mixer).

Color grading status (2026-11-27): three-wheel color grading (shadows, midtones, highlights) plus balance renders in canonical WGSL and the CPU reference as the last step of the shared `color_stage` (HSL, vibrance/saturation, then grading), so Saturation -1 cannot undo a tint. Masks come from the Oklab L reaching the stage: adjacent smoothstep ramps of fixed width 0.30 around the pivot `0.55 - 0.20 * balance`, so the weights are non-negative and sum to one, with positive balance growing highlights. Each wheel's hue is turns of OkLCh hue and chroma 0..1 (precomputed on the CPU to `0.10 * chroma * (cos, sin)` Oklab offsets), applied as a constant additive cast tapered over L 0..0.08 so black and negative L stay untinted; lightness is an additive `0.09 * lightness` lift, with a test enforcing a lightness-map slope of at least 0.1 at every corner (do not raise 0.09 or narrow the width without redoing it). Grading is active only if a wheel has chroma or lightness nonzero; hue-only or balance-only is a no-op, and neutral, exposure-only, HSL-only and vibrance/saturation-only output are unchanged. Uniform rows 12-14 hold the wheels and flags `y`/`w` the active flag and pivot (`w` is written only while grading is active). Validation: hue and chroma 0..1, lightness and balance -1..+1, finite; `EditState::reset_grading` is added and `reset_basic` still preserves grading. The side panel's collapsing **Color grading** (slider phase: Shadows/Midtones/Highlights tabs with hue 0-360 degrees over a rainbow track, chroma, lightness, Balance, per-wheel and global reset; one undo step per gesture; tab is app state) follows the mixer; the 2D wheel widget remains planned. A `film_look.pfpreset.json` fixture (muted greens, warm highlights/cool shadows) covers HSL plus grading round-trip and selective per-band/per-wheel apply. Tests: CPU masks, pivot, monotonic lightness, untinted blacks, inert hue/balance, finite extremes, validation endpoints, row layout and stage order with saturation and curves; software-GPU f32/f16 parity for each wheel at balance -1/0/1, all wheels and grading stacked with tone, curves, vibrance, saturation and HSL, plus grading in the viewport, histogram and probe tests; unit tests for the UI helpers and a headless app test for a slider drag and resets as undo steps. Constants are analysis-based and untuned on real images; defringe remains rejected by the renderer (the channel mixer and monochrome render as of the channel mixer status below), and native desktop interaction and latency remain unverified. See [docs/pointwise.md](docs/pointwise.md#color-grading).

Channel mixer status (2026-11-27): the 3x3 channel mixer and monochrome render in canonical WGSL and the CPU reference as stage 2, with no shader or uniform change: `fold_mixer` computes in f64 `A * M * A^-1 * WB` (rows are output channels in linear sRGB primaries, `A` the repo's linear sRGB to Rec.2020 matrix and `A^-1` its f64 inverse, narrowed to f32 once) into uniform rows 0-2, after exposure and ahead of tone, curves and the Oklab color stage. An identity mixer with monochrome off leaves the white-balance rows untouched, so neutral and every existing path stay bit-identical. Monochrome replaces all three rows with the luminance weights of the mixed color, `Y * M * A^-1 * WB`, where `Y` is the exact linear sRGB luminance `Y_2020 * A` (the spec's rounded 0.2126/0.7152/0.0722 differ by about 4e-5 from the Rec.2020 luminance, so the exact product is used and an identity mixer gives exactly the Rec.2020 luminance weights); the three rows are bit-identical, so R = G = B is bit-identical only through the linear stages; a tone or master-curve Oklab round trip adds a few ulps of noise, and per-channel R/G/B curves intentionally tone the image, after which HSL acts on its real chroma. Rows are never discarded by toggling monochrome. Convention for B&W looks: set all three rows equal to weights summing to one and monochrome on (`EditState::apply_bw_response`); unchecking monochrome in the UI when the three rows are equal also resets the matrix to the identity in the same undo step (otherwise it would leave a gray image), while `reset_mixer` always restores the identity and turns monochrome off. HSL has no effect on a pure mono image (its chroma gate), grading still tints it. Presets (linear sRGB weights, sum 1): Luminance 0.2126/0.7152/0.0722, Panchromatic 0.30/0.58/0.12, Orthochromatic 0.08/0.60/0.32, Yellow filter 0.40/0.55/0.05, Orange filter 0.62/0.35/0.03, Red filter 0.80/0.20/0.00, Green filter 0.10/0.80/0.10. Validation: coefficients in `-2..+2`, finite; defringe must be finite and is the only color control the renderer still rejects when nonzero (along with clarity, detail, finishing and nodes). The side panel's collapsing **Channel mixer** has a Monochrome checkbox; off it shows a 3x3 percent grid with row sums (flagged when more than 1% from 100%), **Normalize rows** (skips zero-sum rows and rows whose normalized coefficients would leave the range) and **Reset channel mixer**; on it shows three gray-weight sliders (`Y * M`, edits write all three rows equal) and the seven preset buttons, each one undo step. A `film_look_mono.pfpreset.json` fixture (orange filter plus split-tone grading) covers the round trip and selective apply. Tests: fold order against an independent linear sRGB reference, identity skip bit-exact, mono equal to Rec.2020 luminance for the identity, bit-identical mono channels, neutral preservation by unit-sum rows, preset sums, validation endpoints, `reset_mixer`, `apply_bw_response`, the unsupported-clone final state (defringe rejected), UI helper tests and a headless app test for preset, uncheck and reset undo steps; software-GPU f32/f16 parity for a general matrix, monochrome and every preset, and stacks with white balance, tone, curves, HSL, vibrance/saturation and grading, plus the mixer in the viewport, histogram and probe tests. The WGSL cube root is Newton-refined to match the CPU `cbrt`, so monochrome with per-channel curves and HSL is in the parity set; a CPU conditioning test perturbs the Oklab cube roots by one ulp (relative 1.2e-7) and bounds the HSL output change by 2e-5*(1+|x|); about 2.5 ulp of root error can exceed that bound with strongly opposed adjacent bands, so the Newton step is required for parity. Native desktop interaction and WARP/DX12 parity remain unverified. See [docs/pointwise.md](docs/pointwise.md#channel-mixer-and-monochrome).

* **Done Criteria:** All Layer 1 filters work smoothly in preview. Image-scale filters match full-res export at fit zoom, and pixel-scale filters match export exactly in the 100% region view. Golden-image tests cover every spatial filter.

### Phase 4: Composable Custom Stack & Preset Ecosystem
* **Deliverables:**
  * Dynamic `Vec<FilterNode>` custom stack with insertion points (reorder, enable/disable, opacity, blend modes: Normal, Multiply, Screen, Overlay, Soft Light in the specified blend space).
  * Custom nodes: Levels, Gaussian Blur, 3x3/5x5 Convolution Kernels, Color Matrix, Duotone/Gradient Map, Bloom/Halation (mip-pyramid), Radial/Linear Gradient Masks, Film Grain, Posterize/Dither, 3D LUT.
  * `.cube` 3D LUT parser (import, sRGB-wrapped) and lattice baker (export with spatial-stage notice).
  * Film look groups with a single **Amount**, monochrome spectral-response presets and generic-named starter film looks (see [Film Emulation Looks](#film-emulation-looks)).
  * Left-dock Preset Browser with OS-standard storage, search/tags, selective apply checkboxes, inline embedded LUTs in shared presets, debounced GPU thumbnails, and starter presets in `assets/presets/`.
* **Done Criteria:** Can create complex multi-stage custom filters, save portable full or partial presets, browse with live thumbnails, and export `.cube` LUTs.

### Phase 5: Polish, `filter-cli`, & Cross-Platform Packaging

Implementation status (2026-11-16): `.github/workflows/ci.yml` now defines Ubuntu 24.04/lavapipe (Vulkan) and Windows 2025/WARP (DX12) jobs for stable-Rust formatting, strict Clippy, locked workspace tests and every ignored software-GPU test, including the 36.6MP export/resizing regression. Software-GPU tests honor backend selection and log their adapters; absent adapters fail rather than skip. GPU tests run serially to bound memory/device pressure. Read-only permissions, immutable action pins, main-only cache saves, timeouts and cancellation of superseded runs are configured. Release packaging, CLI work and native desktop checks remain pending.

SonarQube CI status (2026-11-17): a separate 30-minute Ubuntu job generates LCOV from locked workspace tests, including ignored software-GPU regressions on lavapipe, with pinned `cargo-llvm-cov` and publishes source analysis/coverage to the existing SonarQube Cloud project. Fork and Dependabot PRs run coverage without authenticated analysis; trusted PRs, main pushes and manual dispatch can scan. Rust and weekly Sonar package caches save only from main. Unit-test modules live under each crate's `tests/unit` directory, excluded from production coverage and detected as tests; scan arguments pin the SonarCloud endpoint and pass an absolute LCOV path for Cargo module analysis. The Linux/Windows jobs retain all ignored GPU and large-image regressions; only the large-image export regression is outside the coverage report (since PR #14, which added GPU tests to coverage). GitHub-hosted scanning, coverage import and automatic unit-test classification are verified in PR #13; the SonarCloud quality gate passes.
* **Deliverables:**
  * Add `filter-cli` for batch applying `.pfpreset.json` files to folders of images from the terminal (headless `wgpu`, software adapter when no GPU).
  * Keyboard shortcuts, `eframe` window/settings persistence, optional `.pfedit.json` sidecars, better error toasts, VRAM caps tuning, and optional lossy WebP feature (`webp`).
  * GitHub Actions CI building release binaries for Linux (`x86_64-unknown-linux-gnu`) and Windows (`x86_64-pc-windows-msvc`), running `cargo test` including GPU tests. Hosted runners have no GPU, so install Mesa **lavapipe** (`mesa-vulkan-drivers`) on Linux and use **WARP** (DX12) on Windows.
* **Done Criteria:** CI is green on both platforms with GPU tests running on software adapters; `filter-cli` output is byte-identical to app export for the same preset and image (dithering seed fixed); release binaries start on a clean Linux and Windows machine.

### Phase 6 (Future): Camera RAW & Shared Crate Extraction
* **Deliverables:**
  * Resolve RAW decoder licensing (§8), then implement `RawDecoder` behind `feature = "raw"` in `filter-io`.
  * Optionally extract `egui-image-viewport` / `filter-core` hooks for reuse in procedural image generation projects.
* **Done Criteria:** Common camera RAW files (at least one Canon, Nikon, Sony, and Fujifilm model plus DNG) open into the same pipeline with no changes to `filter-core`/`filter-gpu` stage code, and existing presets apply to them unchanged.

---

## 10. Verification & Testing Strategy

* **Color Math & Safety Unit Tests:** Verify transfer-function round-trips (including mirrored negatives), `moxcms` ICC conversion (Display P3 -> linear Rec.2020 **without clipping** out-of-sRGB colors), Oklab/OkLCh signed `cbrt` with negative inputs (no `NaN`s), and monotone spline bounds/HDR extrapolation.
* **CPU / GPU Parity Tests (pointwise stages only):** Run synthetic gradients, negative/HDR patches, and alpha images through `filter-core::cpu_eval` and headless `filter-gpu` (software adapter in CI) with dithering off, and assert both linear `f32` tolerance and quantized sRGB `max_abs_diff < 2.0 / 255.0` (watching near-black amplification under `Rgba16Float`).
* **Golden-Image Tests (spatial stages):** Spatial filters exist only in WGSL. Hardware bilinear filtering often has only ~8 bits of sub-texel weight precision, so exact CPU parity isn't meaningful. Instead, compare GPU output against stored golden images with a perceptual tolerance (ΔE in Oklab), and test tile-seam invariance: tiled output must equal untiled output within tolerance.
* **Metadata & Preset Compatibility Tests:**
  * Verify EXIF orientation is applied on load and reset (`Orientation = 1`) on export so reloading never double-rotates; verify ICC is embedded in every format.
  * Keep snapshot JSON fixtures from `schema_version: 1` onward. Test partial preset deserialization (`None`/omitted vs explicit `0.0`), per-section merge rules, migrations, and round-tripping of `Unknown` nodes.
  * Keep golden renders per `process_version` so algorithm changes can't silently alter old presets.
* **Edge-Case Checks:**
  * Images with alpha channels (no dark edge halos in spatial filters; clean premultiplied display vs. straight file export; JPEG flattening).
  * Grayscale, 16-bit PNGs, wide-gamut ICC JPEGs, LUT-based ICC profiles (clamped, logged), and graceful errors on unsupported color types (e.g., CMYK if not converted by decoder/CMS).
  * Very wide panoramas or `>8K` / `>36MP` images triggering tiled render and readback without GPU OOM or exceeding `max_buffer_size`.
  * Software-adapter runs (`--software-gpu`) and the GL-backend refusal path.
  * HiDPI display scaling (`pixels_per_point`) and non-ASCII file paths on Windows & Linux.

---

## 11. Decision Log

| Decision | Choice | Rationale |
| :--- | :--- | :--- |
| Rust edition / resolver | Edition 2024, `resolver = "3"` | Current stable defaults. |
| Working color space | Linear Rec.2020 (D65) | Holds wide-gamut sources and future RAW without large negatives. |
| Canonical renderer | GPU (`filter-gpu`); CPU only for pointwise stages | Avoids writing every filter twice; spatial CPU/GPU parity isn't achievable at 2/255. |
| Fallback without working GPU | `wgpu` software adapter (lavapipe / WARP) | eframe's UI needs `wgpu` anyway. |
| Viewport rendering | `egui_wgpu::CallbackTrait` | One pass for zoom sampling, split wipe, checkerboard, clipping, output transform. |
| Preview resolution | 2048px proxy + full-res region path at ≥100% | Pixel-scale effects must be judged at full resolution. |
| GPU compute style | Sampled read + write-only `Rgba16Float` storage, or fragment passes | Portable across Vulkan/DX12 drivers. |
| Histogram | GPU workgroup atomics + double-buffered `map_async` | Non-blocking, no CPU proxy copy. |
| Export device | eframe's device, one tile per submit | Avoids a second device's VRAM; keeps UI responsive. |
| White balance adaptation | CAT16 | Modern, well-behaved; Bradford unnecessary. |
| Tone curve LUT size | 1024 entries | Smooth with HDR extrapolation; trivial cost. |
| Proxy texture format | `Rgba16Float` (`Rgba32Float` if `FLOAT32_FILTERABLE` granted) | Broad support. |
| Source storage | `f16` RGBA | Halves memory vs `f32`. |
| Highlights/Shadows (v1) | Pointwise | Keeps tone LUT-bakeable; local version later via `process_version`. |
| Film grain scale | Frame-relative (image diagonal) by default; output-pixel option | Film looks must keep the same grain character at any export size. |
| Film looks | Layer 2 node group with one Amount | Applying a look must not overwrite the user's basic corrections. |
