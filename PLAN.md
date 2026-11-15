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
| **CPU Image I/O & Color Management** | **`image`** + **`moxcms`** (with `extended_range` feature) + **`rayon`** | `image` decodes/encodes JPEG, PNG (8/16-bit), TIFF, and lossless WebP, and embeds ICC (all four) and EXIF (JPEG/PNG/WebP); `moxcms` (pure Rust) transforms embedded ICC profiles into the working space; `rayon` powers pointwise CPU math, the LUT bake, and decode/encode work. *(Optional `webp` / `libwebp` feature for lossy WebP export.)* |
| **EXIF Editing** | **`little_exif`** (or a small hand-written IFD0 patcher) | `image` can embed EXIF bytes but cannot edit them; rewriting `Orientation = 1` requires parsing the TIFF structure. |
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
   * **Linear RGB:** Exposure (`rgb * exp2(ev)`), relative White Balance (CAT16 chromatic adaptation from D65), Noise Reduction, Gaussian Blur, Unsharp Mask / Sharpening, Vignette, Film Grain, Bloom/Halation.
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
   * Use explicit encoders (`JpegEncoder::new_with_quality`, `PngEncoder` 8/16-bit, `TiffEncoder`, `WebPEncoder::new_lossless`) rather than `save_with_format`.
   * **EXIF:** parse `exif_bytes` with `little_exif` (or an IFD0 patcher), rewrite `Orientation` to `1`, and embed with `ImageEncoder::set_exif_metadata` (JPEG/PNG/WebP). `image` cannot write EXIF to TIFF; v1 drops EXIF on TIFF export or writes it via the `tiff` crate directly.
7. **Memory Budget:**
   * `Vec<[f32; 4]>` is 16 B/px (800 MB for a 50MP source). Store the immutable full-res source as **`f16` RGBA (8 B/px)** in `filter-core::buffer`, converting to `f32` per tile on the CPU paths. Set a soft memory cap from the start (not Phase 5) and warn before opening images that exceed it.

### B. Preview Proxy, Full-Res Region Rendering, `egui_wgpu` Viewport, & Tiled Export
* **Async Decode & Immutable Full-Res Source:** Decode on a background worker, keep a single immutable `Arc<DecodedImage>`, and immediately generate a downscaled preview proxy (area-filtered in linear light, longest edge `2048px`). Upload the proxy once as `Rgba16Float` (or `Rgba32Float` when `FLOAT32_FILTERABLE` was requested and granted).
* **Two Render Paths — Proxy and Full-Res Region:** The proxy cannot show pixel-scale effects: a 1px sharpen at 24MP is about 0.35px at 2048px, so sharpening, grain, and NR can't be judged on it. Therefore:
  * **Fit / zoomed-out:** render the whole proxy.
  * **Zoom ≥ proxy scale (e.g. 100%):** render the visible region from the full-res source, in cached tiles that are invalidated on parameter change. Show the upscaled proxy while region tiles render (progressive refinement), then swap in the full-res tiles.
  * Both paths use the same `filter-gpu` executor with a global-UV offset/scale, so they are the same code as tiled export.
* **Pixel-Scale vs Image-Scale Operations:** Every stage/op declares its scale in `pipeline.rs`:
  * **Image-scale** (defined in normalized units of the image diagonal; preview matches export): blur, bloom/halation, clarity radius, vignette, gradient masks, NR strength.
  * **Pixel-scale** (defined in *output* pixels; exact only in the full-res path, and the proxy shows a band-limited approximation): sharpening radius, film grain size, convolution kernels (3×3/5×5), posterize/ordered dither patterns. Grain hashes integer *output-pixel* coordinates; at proxy scale it renders the statistically equivalent downsampled grain (reduced amplitude) instead of hashing proxy pixels.
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
2. **Input / Calibration (linear):** Exposure (`-5.0..+5.0` EV), White Balance (relative Temperature/Tint shift from D65 via CAT16).
3. **Tone (perceptual):** Brightness (midtone lightness/gamma lift preserving black and white points, distinct from linear EV Exposure), Contrast, Highlights, Shadows, Whites, Blacks. **v1 decision:** Highlights/Shadows are pointwise (curve-based), so this stage stays LUT-bakeable; a locally adaptive version can come later under a new `process_version`.
4. **Local Contrast (spatial):** Clarity (pyramid-based).
5. **Tone Curves (perceptual):** Master curve + R, G, B curves (monotone cubic Hermite spline baked to a 1024-entry 1D LUT with linear extrapolation `> 1.0`; R/G/B evaluate in sRGB primaries).
6. **Color (perceptual):** Vibrance, Saturation, 8-band HSL (Red, Orange, Yellow, Green, Aqua, Blue, Purple, Magenta; OkLCh hue), 3-way Color Grading (Shadows/Midtones/Highlights color + balance in Oklab/OkLCh), Channel Mixer / Monochrome toggle, Defringe (purple/green fringe desaturation).
7. **Detail (linear):** Sharpening / Unsharp Mask (amount, radius, detail threshold) — pixel-scale.
8. **Finishing:** 3D LUT overlay (`.cube` wrapped in sRGB + strength), Vignette (amount, midpoint, roundness, feather; relative to the crop rect), Procedural Film Grain (amount, size, roughness—driven by a deterministic integer spatial hash in output-pixel coordinates).
9. **Output (export only):** Resize, output sharpening, gamut compression, transfer/ICC, dither, quantize.

Because Clarity (4) sits between pointwise stages, `.cube` export bakes pointwise Layer 1 stages (2, 3, 5, 6, and Finishing LUT) together with pointwise Layer 2 `FilterNode`s in stage order—skipping Clarity and any spatial stages/nodes—and applies soft gamut compression to sRGB; the export dialog lists any active skipped stages (see §6).

### Layer 2: Extensible Custom Filter Stack (`Vec<FilterNode>`)
For users who want to go beyond standard sliders and craft complex/artistic looks, add a user-editable stack of composable nodes:
* **Node wrapper:** `{ id, name, enabled: bool, position: StackPosition, blend_mode: BlendMode, opacity: f32, op: FilterOp }`
* **Insertion points (`StackPosition`):** `AfterCalibration` (linear, after stage 2), `AfterColor` (after stage 6), `AfterFinishing` (after stage 8). Nodes run in list order within each position.
* **Supported `FilterOp` variants** (scale class in brackets):
  * `Levels { in_black, gamma, in_white, out_black, out_white }` [pointwise]
  * `CustomCurve { channel, points: Vec<[f32; 2]> }` [pointwise]
  * `ColorMatrix3x3 { matrix: [[f32; 3]; 3], bias: [f32; 3] }` [pointwise]
  * `GaussianBlur { radius: f32 }` [image-scale] (mip-pyramid blur with shared coarse proxy levels)
  * `ConvolutionKernel { size: 3 | 5, weights: Vec<f32>, normalize: bool }` [pixel-scale] (custom emboss, edge enhance, high-pass, custom blur)
  * `Duotone / GradientMap { shadows_color, highlights_color }` [pointwise]
  * `SelectiveColor / HueRangeMask` [pointwise]
  * `RadialOrLinearGradientFilter { geometry, feather, exposure_delta, tint, saturation }` [image-scale; geometry relative to crop rect]
  * `Bloom / Halation { threshold, radius, intensity }` [image-scale]
  * `Posterize / OrderedDither` [pointwise / pixel-scale] (great overlap with procedural art)
  * `FilmGrain { amount, size, roughness }` [pixel-scale] (available as a stackable node as well as in Finishing)
  * `Lut3D { lut: LutSource, strength }` [pointwise] (built-in ID, content-hashed library LUT, or inline embedded LUT payload)
  * `Unknown(serde_json::Value)` — preserves nodes from newer versions so they round-trip; rendered as a disabled pass-through with a warning badge.

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
  * **Slider Ergonomics & Undo Batching:** Double-click any slider or label to reset to default; hold `Shift` for fine precision; numeric entry on double-click. Capture the pre-drag snapshot on `response.drag_started()` and commit an undo entry only on `response.drag_stopped()` (or focus loss / reset click), never per-frame during `.changed()`. Snapshots share LUT payloads via `Arc`.
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
* **Deliverables:**
  * Initialize Cargo workspace (`Cargo.toml` with `resolver = "3"` + `filter-core`, `filter-gpu`, `filter-io`, `filter-app`) with aligned `wgpu` version.
  * `eframe` desktop window launching cleanly on Linux and Windows, with a custom `device_descriptor` requesting adapter limits/features and `Backends::PRIMARY`.
  * Background-threaded decode (`rfd` or drag-and-drop) handling EXIF orientation + `moxcms` (`extended_range`) ICC conversion into `Arc<DecodedImage>` (linear Rec.2020, `f16`), generating a `2048px` proxy before GPU upload.
  * **GPU spike (de-risking):** one exposure shader in `filter-gpu`, drawn through a `CallbackTrait` viewport on eframe's device, plus a tiled full-res render + non-blocking `map_async` readback of a >36MP image (respecting `256`-byte `COPY_BYTES_PER_ROW_ALIGNMENT` row padding).
* **Done Criteria:** Can open a large wide-gamut/rotated JPEG or PNG on both Linux and Windows and pan/zoom smoothly at 60fps. Display P3 test colors survive the load without clipping. The exposure spike exports a >36MP image via tiled readback without exceeding device limits.

### Phase 1: `filter-core` Data Model, Pointwise Engine & Basic Export
* **Deliverables:**
  * Define `EditState` (concrete), `PresetPatch` (`Option`-wrapped) with merge rules, `process_version`, the `crop` rect, and color math (transfer functions, Rec.2020 matrices, Oklab with signed `cbrt`, CAT16) in `filter-core`.
  * Implement pointwise stages (Exposure, White Balance, Brightness, Contrast, Highlights/Shadows, Whites/Blacks, Vibrance, Saturation) **in WGSL as the canonical renderer**, plus the matching `rayon` CPU implementation in `cpu_eval` (used for LUT bake, probes, and parity tests).
  * Wire right-side sliders to the GPU proxy render, plus background export using explicit `image` encoders (JPEG quality, 8/16-bit PNG, lossless WebP, TIFF), resize, alpha flattening for JPEG, triangular 8-bit dithering, ICC embedding, and EXIF `Orientation = 1` rewriting.
  * Implement preset JSON save/load with schema migration scaffolding and `Unknown` node preservation.
* **Done Criteria:** Non-destructive edits work end-to-end; presets save/reload identically (including partial presets and unknown future nodes); exports preserve color accuracy and correct orientation; unit tests cover color conversions, merge rules, and preset schema round-trips.

### Phase 2: Real-Time Pipeline, Full-Res Region Rendering & Job System
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
* **Done Criteria:** All Layer 1 filters work smoothly in preview. Image-scale filters match full-res export at fit zoom, and pixel-scale filters match export exactly in the 100% region view. Golden-image tests cover every spatial filter.

### Phase 4: Composable Custom Stack & Preset Ecosystem
* **Deliverables:**
  * Dynamic `Vec<FilterNode>` custom stack with insertion points (reorder, enable/disable, opacity, blend modes: Normal, Multiply, Screen, Overlay, Soft Light in the specified blend space).
  * Custom nodes: Levels, Gaussian Blur, 3x3/5x5 Convolution Kernels, Color Matrix, Duotone/Gradient Map, Bloom/Halation (mip-pyramid), Radial/Linear Gradient Masks, Film Grain, Posterize/Dither, 3D LUT.
  * `.cube` 3D LUT parser (import, sRGB-wrapped) and lattice baker (export with spatial-stage notice).
  * Left-dock Preset Browser with OS-standard storage, search/tags, selective apply checkboxes, inline embedded LUTs in shared presets, debounced GPU thumbnails, and starter presets in `assets/presets/`.
* **Done Criteria:** Can create complex multi-stage custom filters, save portable full or partial presets, browse with live thumbnails, and export `.cube` LUTs.

### Phase 5: Polish, `filter-cli`, & Cross-Platform Packaging
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
