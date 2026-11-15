# Photo Filter & Preset Studio — Architecture & Implementation Plan

A cross-platform (Linux & Windows) native desktop photo editor focused on fast, non-destructive adjustments, detailed composable filters, and shareable presets—written in Rust with a modular workspace so core components can overlap with a procedural image generator and support camera RAW in the future.

---

## 1. Goals & Scope

### Core Features (v1)
* **Simple & Responsive Adjustments:** Exposure, contrast, brightness, highlights/shadows, whites/blacks, white balance (temperature/tint + eyedropper), vibrance, and saturation.
* **Detailed & Custom Filters:** Interactive tone curves (RGB + per-channel), 8-band HSL mixer, 3-way color grading wheels, sharpening/unsharp mask, clarity/local contrast, blur, vignette, procedural film grain, custom convolution kernels, 3D LUT import (`.cube`), and an extensible ordered filter stack with per-filter mix/blend modes.
* **Presets & Export:** Save/load full or partial filter pipelines as versioned presets, selective/partial preset application, live preset thumbnail previews, `.cube` 3D LUT export, and full-resolution image export (JPEG with quality, 8/16-bit PNG, lossless WebP—plus optional lossy WebP, TIFF) with safe overwrite confirmation.
* **Desktop UX:** Instant preview updates while dragging sliders, pan/zoom canvas, split-screen before/after compare, live non-blocking RGB/luminance histogram, drag-batched undo/redo history, and native OS file dialogs on Linux and Windows.

### Non-Goals & Out of Scope
* **Camera RAW Decoding:** Deferred to a later phase, with a clean decoder boundary in `filter-io` so RAW support drops in without changing the filter engine or UI.
* **Destructive Brush/Mask Painting & Layer Compositing:** Out of scope for v1 (though procedural radial/linear/luminance masks inside filter nodes fit cleanly).
* **Geometry Edits (Crop / Straighten / Perspective Warp):** Out of scope; only EXIF auto-orientation is applied on load so photos open upright.

---

## 2. Technology Stack

| Layer | Choice | Why |
| :--- | :--- | :--- |
| **Language** | **Rust** (Edition 2024, or 2021 with `resolver = "2"`) | Single fast native binaries on Linux & Windows; natural overlap with a Rust procedural image project. |
| **GUI Framework** | **`eframe` + `egui`** (default `wgpu` backend, `persistence` feature) | Immediate-mode UI makes custom interactive widgets (tone curves, color wheels, histograms, split compare) straightforward and runs identically on Linux & Windows. |
| **GPU Acceleration** | **`wgpu`** | Runs natively on Vulkan (Linux) and DX12/Vulkan (Windows). Enables sub-5ms shader previews for complex filter chains. |
| **CPU Image I/O & Color Management** | **`image`** + **`moxcms`** + **`rayon`** | `image` decodes/encodes JPEG, PNG (8/16-bit), TIFF, and lossless WebP; `moxcms` (pure Rust) transforms embedded ICC profiles (Display P3, Adobe RGB, etc.); `rayon` powers parallel CPU reference filters, thumbnails, and fallback exports. *(Optional `webp` / `libwebp` feature can be added if lossy WebP export is required.)* |
| **Native File Dialogs** | **`rfd`** | Native dialogs on Linux (`xdg-portal` pure-Rust backend or `gtk3`) and Win32 dialogs on Windows. |
| **Serialization, IDs & Paths** | **`serde`**, **`serde_json`**, **`uuid`**, **`directories`** | Versioned, human-readable preset files stored in standard OS config/data folders. |
| **GPU Interop Helpers** | **`bytemuck`** | Safe casting of uniform structs to WGSL shader buffers. |

> **Important Dependency Note (`eframe` + `wgpu`):**  
> `eframe` pulls in `egui-wgpu` pinned to a specific `wgpu` version. Pin that exact `wgpu` version in the root `[workspace.dependencies]` so `filter-gpu` and `filter-app` share identical `wgpu::Device`, `wgpu::Queue`, and `wgpu::TextureView` types without duplicate crate conflicts. **Do not** re-export `eframe::wgpu` from `filter-gpu`—keeping `filter-gpu` independent of `eframe` lets `filter-cli` and headless libraries use the GPU engine without compiling `egui`.

### Linux Build Prerequisites
`eframe`/`winit` on Linux require standard windowing/xkb dev headers (e.g., on Debian/Ubuntu):
```bash
sudo apt-get install libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libssl-dev
```
* **`rfd` Backend Choice on Linux:** Prefer `rfd` with `default-features = false, features = ["xdg-portal"]` (plus an async executor feature) for native XDG desktop portal dialogs without C GTK headers; if using `rfd`'s default `gtk3` backend instead, also install `libgtk-3-dev` and `pkg-config`.
* Windows (`x86_64-pc-windows-msvc`) requires no extra external SDKs beyond standard MSVC build tools.

---

## 3. Workspace Architecture (Shared Core & Reusable Overlap)

Splitting the codebase into a Cargo workspace keeps the GUI, GPU shaders, I/O, and filter math cleanly decoupled. This directly benefits headless CLI workflows and procedural image generators:

```text
photo-filter/
├── Cargo.toml                        # Workspace manifest (resolver = "2", shared [workspace.dependencies])
├── PLAN.md
├── assets/
│   └── presets/                      # Built-in starter presets (.pfpreset.json)
└── crates/
    ├── filter-core/                  # Headless pure-Rust crate (NO GUI or wgpu deps)
    │   ├── Cargo.toml
    │   └── src/
    │       ├── lib.rs
    │       ├── buffer.rs             # ImageBufferF32 ([f32; 4] linear sRGB RGBA) + downsampling/tiling
    │       ├── color.rs              # sRGB <-> Linear, Oklab/OkLCh (signed cbrt), gamut map, HSL, CAT16/Bradford
    │       ├── pipeline.rs           # Serializable FilterPipeline, FilterNode, typed params
    │       ├── cpu_eval.rs           # Deterministic rayon CPU implementation (reference + fallback)
    │       ├── histogram.rs          # Fast RGB/Luma histogram & clipping stats
    │       ├── lut.rs                # .cube 3D LUT parser & generator/exporter
    │       └── preset.rs             # Versioned preset format, Option-based partial apply, embedded LUTs
    │
    ├── filter-gpu/                   # wgpu compute/render pipeline matching filter-core specs
    │   ├── Cargo.toml
    │   └── src/
    │       ├── lib.rs
    │       ├── context.rs            # Manages wgpu Device/Queue (borrowed from eframe OR headless wgpu)
    │       ├── shaders/              # WGSL shaders (color/tone, curves, mip-pyramid blur/bloom, grain, 3D LUT)
    │       └── executor.rs           # Ping-pong offscreen textures, uniform packing, full-res & tiled export
    │
    ├── filter-io/                    # Decoding, encoding, ICC color management, EXIF, future RAW hook
    │   ├── Cargo.toml
    │   └── src/
    │       ├── lib.rs
    │       ├── decoder.rs            # ImageDecoder trait -> DecodedImage (linear Rgba32F + ICC/EXIF metadata)
    │       ├── standard.rs           # JPEG/PNG/WebP/TIFF via `image` + ICC transform via `moxcms`
    │       ├── encoder.rs            # Explicit format encoders, dithering, EXIF Orientation=1 normalization
    │       └── raw.rs                # (Future Phase) RawDecoder behind `feature = "raw"`
    │
    ├── filter-app/                   # Desktop GUI binary (eframe + egui)
    │   ├── Cargo.toml
    │   └── src/
    │       ├── main.rs               # eframe entry, icon, persistence, native options
    │       ├── app.rs                # App state, background decode/export/thumbnail workers, undo/redo
    │       ├── preview.rs            # Proxy texture management, eframe/egui_wgpu bridge, non-blocking readback
    │       ├── history.rs            # Drag-batched snapshot undo-redo
    │       └── ui/
    │           ├── top_bar.rs        # File ops, zoom, before/after toggle, clipping warnings, export dialog
    │           ├── viewport.rs       # Pan/zoom canvas, PaintCallback/split wipe, checkerboard alpha, eyedropper
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
1. **`filter-core` + `filter-gpu` as a Post-Processing Library:** A procedural generator producing raw pixel buffers (`Vec<[f32; 4]>` or `ImageBufferF32`) can depend directly on `filter-core` (or `filter-gpu` headlessly) to apply color grading, film grain, bloom, tone curves, or `.cube` LUTs via CLI without compiling `egui` or `eframe`.
2. **Shared Viewport Widget Potential:** `viewport.rs` (pan/zoom, split-screen compare, checkerboard alpha, cursor probe) depends only on `egui` + texture/callback handles, so it can be extracted into a shared `egui-image-view` crate later.

---

## 4. Color Space & Rendering Pipeline

Getting the math, alpha discipline, and GPU/CPU boundaries right early prevents banding, NaN artifacts, dark transparent fringes, and mismatched exports:

### A. Internal Precision, Color Management, & Alpha Discipline
1. **Input Normalization & ICC Color Management:**
   * Decode 8-bit/16-bit images via `image`, apply EXIF orientation (`DynamicImage::apply_orientation`) so pixels are upright in memory, and extract `icc_profile` + `exif_metadata`.
   * If an embedded ICC profile is present (e.g., Display P3, Adobe RGB), use `moxcms` to transform into working **Linear sRGB** `Rgba32F` (`[f32; 4]`); if untagged, convert `sRGB -> Linear sRGB`.
2. **HDR Headroom & Safe Math:**
   * Keep intermediate working values unclamped (`f32`, allowing values `> 1.0` or `< 0.0` transiently during exposure, sharpening, or matrix ops).
   * **Signed Cube Root in Oklab:** Because negative intermediate values can occur, always implement cube root in both Rust and WGSL as `sign(x) * pow(abs(x), 1.0 / 3.0)` (never raw `pow(x, 1.0/3.0)`, which produces `NaN` in WGSL for `x < 0.0`).
   * Apply soft gamut mapping / compression (e.g., OkLCh chroma clamping toward the sRGB gamut boundary) before final display/export clamping to avoid hue skews on oversaturated blues/magentas.
3. **Space-Aware Operations & Transfer Boundaries:**
   * **Linear RGB:** Exposure (`rgb * exp2(ev)`), relative White Balance (Bradford/CAT16 chromatic adaptation from D65), Gaussian Blur, Unsharp Mask / Sharpening, Vignette, Film Grain, Bloom/Halation.
   * **Perceptual / sRGB / Oklab / OkLCh / HSL:** Contrast S-curve, Highlights/Shadows/Whites/Blacks, Tone Curves, Vibrance, Saturation, 8-band HSL, and 3-way Color Grading.
   * **Tone Curve Domain:** Evaluate 1D Tone Curves in perceptual/sRGB domain (`linear -> sRGB -> 1D LUT -> linear`), using linear slope extrapolation above `1.0` so control points feel evenly distributed across shadows/midtones/highlights while preserving HDR headroom.
   * **`.cube` 3D LUT Domain:** Standard creative `.cube` LUTs are sRGB/display-referred, not linear-referred. Wrap `.cube` evaluation in `linear -> sRGB (mapped by DOMAIN_MIN..DOMAIN_MAX) -> 3D LUT -> linear`.
4. **Alpha Handling (Premultiplied vs. Straight):**
   * Store **straight (unmultiplied)** alpha in `DecodedImage` and final file export buffers (`PNG`, `WebP`, `TIFF`).
   * Around any spatial filter (`Blur`, `Sharpen`, `Convolution`, `Bloom`), temporarily premultiply RGB by alpha (or weight taps by alpha) and unpremultiply afterward so transparent `(0, 0, 0, 0)` neighbors never bleed dark fringes into opaque edges.
   * Because `egui` / `egui_wgpu` expects **premultiplied alpha** in gamma-space `Rgba8Unorm` textures, premultiply RGB by alpha when writing the final viewport display texture.
5. **Output Transform & Export Metadata:**
   * Convert working Linear -> output sRGB (or target ICC profile via `moxcms`) right before display or file export.
   * Apply subtle triangular dithering when quantizing down to 8-bit (`Rgba8Unorm` display or 8-bit JPEG/PNG/WebP export) to prevent gradient banding; skip dithering for 16-bit PNG/TIFF, and allow disabling dithering in deterministic parity tests.
   * Use explicit encoders (`JpegEncoder::new_with_quality`, `PngEncoder` 8/16-bit, `TiffEncoder`, `WebPEncoder::new_lossless`) rather than `Rgba32FImage::save_with_format`. When writing `exif_bytes` back to exported files, **rewrite or strip the EXIF `Orientation` tag to `1` (Normal)** so already-oriented pixels are not rotated a second time by viewers.

### B. Preview Proxy, `egui_wgpu` Viewport, & Tiled Export
* **Async Decode & Immutable Full-Res Source:** Decode images on a background worker thread, keep a single immutable `Arc<DecodedImage>` in memory, and immediately generate a downscaled linear `f32` preview proxy (`max 1600px–2048px` on the longest edge). Upload this proxy once as a `wgpu::Texture` (`Rgba16Float`, or `Rgba32Float` when `FLOAT32_FILTERABLE` is available).
* **WebGPU-Safe Ping-Pong Textures:** Use `Rgba16Float` intermediate textures. Because WebGPU guaranteed features provide write-only `STORAGE_BINDING` (not `read_write`) on `Rgba16Float`, structure compute passes to read from `texture_2d<f32>` (sampled) and write to a ping-pong `texture_storage_2d<rgba16float, write>`, or use fullscreen fragment render passes (`RENDER_ATTACHMENT`).
* **Viewport Rendering & Texture Lifecycle (`egui_wgpu`):**
  * Render `filter-gpu`'s final UI output into an off-screen `wgpu::TextureFormat::Rgba8Unorm` texture (the format required by `egui_wgpu::Renderer::register_native_texture`), updating it only when pipeline parameters or the image change.
  * When loading a new image with different dimensions, properly free the old `TextureId` (`renderer.free_texture`) or replace it via `renderer.update_egui_texture_from_wgpu_texture` to avoid VRAM leaks.
  * To avoid zoomed-out aliasing/shimmering on fine grain/sharpening and zoomed-in blurriness at `>100%`, either generate mipmaps for the display texture + switch sampling to `Nearest` above `100%`, or render the canvas via an `egui_wgpu::CallbackTrait` (`PaintCallback`) quad that handles zoom sampling, checkerboard alpha, Before/After split wipe, and clipping overlays directly on the GPU in one pass.
* **Non-Blocking Histogram & Cursor Readout:** Never call `device.poll(Wait)` on the UI thread. Compute the live histogram either via a GPU compute atomic pass into a `256 x 4 x u32` buffer read back through double-buffered async `map_async` staging buffers (1-frame latency), or asynchronously on CPU from a tiny `~256–512px` proxy. Use the same non-blocking path (or local patch `cpu_eval`) for cursor pixel RGB/Hex/Oklab readout.
* **Resolution-Independent Spatial Parameters & Tiled Full-Res Export:**
  * Express blur/sharpen radii, vignette falloff, and grain size relative to the image's diagonal (normalized UV units) so `2048px` preview matches `6000px+` full-res export.
  * Run full-res exports on a background thread using headless `wgpu` (or `filter-core` `rayon` CPU fallback), tiling into e.g. `2048x2048` tiles when exceeding `limits().max_texture_dimension_2d`.
  * Pass global image UV offset/scale uniforms into tile shaders so Vignette, Gradient Masks, and Film Grain remain seamless across tile borders. Implement Bloom and large blurs via a **downsample/upsample mip pyramid** (e.g., Dual Kawase) so large radii stay fast and tile aprons stay bounded across chained spatial nodes.

---

## 5. Filter Pipeline Specification

Design the pipeline in two complementary layers so simple edits stay fast and intuitive while detailed edits remain limitless:

### Layer 1: Standard Ordered Pipeline (Fixed Fast-Path Stages)
1. **Input / Calibration:** Exposure (`-5.0..+5.0` EV), White Balance (relative Temperature/Tint shift from D65 via Bradford/CAT16 matrix in linear RGB).
2. **Tone:** Contrast, Highlights, Shadows, Whites, Blacks, Clarity / Local Contrast (pyramid-based).
3. **Tone Curves:** Master curve + R, G, B curves (monotone cubic Hermite spline baked to a 1D 256/1024-entry LUT evaluated in perceptual/sRGB domain with linear extrapolation `> 1.0`).
4. **Color:** Vibrance, Saturation, 8-band HSL (Red, Orange, Yellow, Green, Aqua, Blue, Purple, Magenta), 3-way Color Grading (Shadows/Midtones/Highlights color + balance in Oklab/OkLCh), Channel Mixer / Monochrome toggle.
5. **Detail & Optics:** Sharpening / Unsharp Mask (amount, radius, detail threshold), Noise Reduction (fast bilateral / edge-preserving smooth), Chromatic Aberration / Defringe (optional).
6. **Finishing:** 3D LUT overlay (`.cube` wrapped in sRGB + strength), Vignette (amount, midpoint, roundness, feather), Procedural Film Grain (amount, size, roughness—driven by a deterministic integer spatial hash shared identically between Rust and WGSL).

### Layer 2: Extensible Custom Filter Stack (`Vec<FilterNode>`)
For users who want to go beyond standard sliders and craft complex/artistic looks, add a user-editable stack of composable nodes applied in order (or inserted pre/post-color):
* **Node wrapper:** `{ id, name, enabled: bool, blend_mode: BlendMode, opacity: f32, op: FilterOp }`
* **Supported `FilterOp` variants:**
  * `Levels { in_black, gamma, in_white, out_black, out_white }`
  * `CustomCurve { channel, points: Vec<[f32; 2]> }`
  * `ColorMatrix3x3 { matrix: [[f32; 3]; 3], bias: [f32; 3] }`
  * `ConvolutionKernel { size: 3 | 5, weights: Vec<f32>, normalize: bool }` (custom emboss, edge enhance, high-pass, custom blur)
  * `Duotone / GradientMap { shadows_color, highlights_color }`
  * `SelectiveColor / HueRangeMask`
  * `RadialOrLinearGradientFilter { geometry, feather, exposure_delta, tint, saturation }`
  * `Bloom / Halation { threshold, radius, intensity }`
  * `Posterize / OrderedDither` (great overlap with procedural art)
  * `FilmGrain { amount, size, roughness }` (available as a stackable node as well as in Finishing)
  * `Lut3D { lut: LutSource, strength }` (supports built-in ID, content-hashed library LUT, or inline embedded LUT payload)

---

## 6. Preset System & `.cube` Export

### Native Preset Format (`.pfpreset.json`)
```json
{
  "schema_version": 1,
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "name": "Warm Film Fade",
  "author": "user",
  "tags": ["film", "warm", "portrait"],
  "pipeline": {
    "basic": { "exposure": 0.15, "contrast": 0.12 },
    "curves": { "master": [[0.0, 0.05], [0.25, 0.22], [0.75, 0.78], [1.0, 0.95]] },
    "hsl": null,
    "custom_stack": [
      {
        "enabled": true,
        "opacity": 0.4,
        "blend_mode": "Normal",
        "op": { "type": "FilmGrain", "amount": 0.25, "size": 1.2, "roughness": 0.5 }
      }
    ]
  }
}
```
* **Forward Compatibility & Partial Presets:** Use `#[serde(default)]` **together with `Option<...>` wrappers** on sections/fields (not bare `f32` defaults) so a partial preset (`"basic": null` or omitted fields) leaves the active photo's untouched parameters intact, while an explicit `"exposure": 0.0` resets exposure to `0.0`.
* **Self-Contained Sharing:** When exporting/sharing `.pfpreset.json` files that use a custom `.cube` LUT (`Lut3D`), embed the LUT payload (or compressed lattice) inline rather than relying on local filesystem paths.
* **Selective Save / Apply:** Checkboxes when saving or applying a preset (e.g., *Tone*, *Curves*, *Color/HSL*, *Detail/Finishing*, *Custom Stack*) so you can apply a color grade without overriding exposure or white balance.
* **Preset Browser UX:** Store user presets in the OS app-data directory (`directories::ProjectDirs`), allow drag-and-drop import/export of `.pfpreset.json` files or folders, and render `128px` live thumbnails asynchronously on a background CPU `rayon` queue (zero contention with the interactive `wgpu` preview queue).

### `.cube` 3D LUT Import & Export
* **Import:** Parse standard Adobe `.cube` (`LUT_3D_SIZE N`, `DOMAIN_MIN`, `DOMAIN_MAX`, `R` fastest -> `G` -> `B` order), upload as a `wgpu::TextureFormat::Rgba16Float` 3D texture with hardware trilinear sampling (using half-texel inset `(0.5 + x * (N - 1)) / N`) or tetrahedral interpolation in WGSL, wrapped in `linear -> sRGB -> LUT -> linear`.
* **Export:** Sample the pointwise color/tonal stages of the current `FilterPipeline` across an `N×N×N` sRGB `[0, 1]` grid (`linearize -> pointwise pipeline -> linear_to_srgb`, e.g., `N = 33` or `65`) and write a valid `.cube` file (`R` fastest). Display a non-blocking notice in the export dialog if spatial filters (sharpen, blur, vignette, grain, bloom) are active, since 3D LUTs cannot encode spatial effects.

---

## 7. GUI & Interaction Design (`egui` / `eframe`)

### Layout & Ergonomics
* **Top Toolbar:** `Open`, `Export`, `Undo / Redo`, `Reset All`, Zoom (`Fit`, `100%`, `%`), `Before / After` mode (`Hold Space` or `Split Slider`), `Clipping Warnings` toggle (highlight blown whites in red / crushed blacks in blue).
* **Left Panel (Collapsible):**
  * **Presets Tab:** Search bar, tag filter, grid/list of preset cards with live thumbnail previews of the active photo, `+ Save Preset`, `Import`, `Export`.
  * **History Tab:** Edit action log to jump back to any state.
* **Center Canvas:**
  * Smooth pan (middle-drag or `Space` + drag) & cursor-centered scroll-wheel zoom.
  * Draggable vertical split bar for Before/After comparison + checkerboard backdrop for transparent images.
  * Drag-and-drop image files or `.pfpreset.json` / `.cube` files directly onto the window.
* **Right Inspector Panel:**
  * **Live Histogram:** Smooth RGB + Luminance area plot at the top (non-blocking updates), plus pixel RGB/Hex/Oklab readout under cursor and White Balance eyedropper mode.
  * **Collapsible Adjustment Sections:** Basic, Tone Curves (interactive spline editor), HSL / Color Mixer, Color Grading Wheels, Detail & Finishing, and Custom Filter Stack (`+ Add Node`, drag/button reorder, mute/solo, opacity slider).
  * **Slider Ergonomics & Undo Batching:** Double-click any slider or label to reset to default; hold `Shift` for fine precision; numeric entry on double-click. Capture the pre-drag snapshot on `response.drag_started()` and commit an undo entry only on `response.drag_stopped()` (or focus loss / reset click), never per-frame during `.changed()`.
* **Export Safety:** Confirm before overwriting an existing file or source image on export; optionally support per-photo `.pfedit.json` sidecar saving/reloading.

---

## 8. Future-Proofing for Camera RAW & Cross-Platform Reliability

1. **Decoder Trait Boundary (`filter-io`):**
   ```rust
   pub struct DecodedImage {
       pub width: u32,
       pub height: u32,
       /// Linear sRGB working space, straight (unmultiplied) 4xf32 RGBA
       pub pixels: Vec<[f32; 4]>,
       pub orientation: Orientation,
       pub icc_profile: Option<Vec<u8>>,
       pub exif_bytes: Option<Vec<u8>>,
   }
   ```
   Standard formats go through `image::ImageReader` + `moxcms`. Later, enabling a `raw` Cargo feature plugs in a `RawDecoder` (e.g., `rawler` in pure Rust or `libraw` bindings) that demosaics, applies camera matrix + white balance, and emits the exact same `DecodedImage` linear `f32` buffer.
2. **EXIF Orientation & Metadata Hygiene:** Apply EXIF orientation rotation/flip on load so portrait phone/camera photos display upright immediately, and strip/reset `Orientation` to `1` in `exif_bytes` on export.
3. **GPU Compatibility Safeguards:**
   * Check `adapter.limits().max_texture_dimension_2d` (and `max_texture_dimension_3d` for `.cube` LUTs) on startup; clamp preview proxy well below this and tile full-res exports automatically when exceeded.
   * Use `Rgba16Float` intermediate textures (sampled reads + write-only storage or `RENDER_ATTACHMENT` passes) for broad hardware compatibility on Linux Mesa/NVIDIA/AMD Vulkan and Windows DX12, with premultiplied `Rgba8Unorm` for the `egui` display texture.
   * Provide a `--cpu` flag / headless fallback using `filter-core`'s `cpu_eval` in case a user runs on broken GPU drivers or in headless CI.

---

## 9. Phased Implementation Roadmap

### Phase 0: Workspace Bootstrap & Minimal Viewer
* **Deliverables:**
  * Initialize Cargo workspace (`Cargo.toml` + `filter-core`, `filter-gpu`, `filter-io`, `filter-app`) with aligned `wgpu` version.
  * `eframe` desktop window launching cleanly on Linux and Windows.
  * Background-threaded decode (`rfd` or drag-and-drop) handling EXIF orientation + `moxcms` ICC profile conversion into `Arc<DecodedImage>`, generating a `max 2048px` preview proxy before GPU upload so large images never hit texture limits or stall the UI.
* **Done Criteria:** Can open a large wide-gamut/rotated JPEG or PNG on both Linux and Windows and pan/zoom smoothly at 60fps.

### Phase 1: `filter-core` Data Model, CPU Engine & Basic Export
* **Deliverables:**
  * Define `FilterPipeline`, `BasicParams` (`Option`-wrapped for partial presets), linear/sRGB & Oklab (signed `cbrt`) color math in `filter-core`.
  * Implement fast `rayon`-parallel CPU pipeline for Exposure, Contrast, Highlights/Shadows, Whites/Blacks, White Balance, Vibrance, Saturation.
  * Wire right-side sliders to the downscaled preview buffer + background full-res export using explicit `image` encoders (JPEG quality, 8/16-bit PNG, lossless WebP, TIFF), triangular 8-bit dithering, ICC tagging, and normalized EXIF orientation.
  * Implement preset JSON save/load (`filter-core/src/preset.rs`).
* **Done Criteria:** Non-destructive edits work end-to-end; presets save/reload identically (including partial presets); exports preserve color accuracy and correct orientation; unit tests cover color conversions and preset schema round-trips.

### Phase 2: `filter-gpu` (`wgpu` Real-Time Pipeline)
* **Deliverables:**
  * Implement WGSL shader pipeline (`filter-gpu`) matching `filter-core` math (sampled reads + write-only `Rgba16Float` ping-pong or fragment passes, premultiplied `Rgba8Unorm` display output).
  * Display off-screen output in `eframe` via `egui_wgpu::Renderer::register_native_texture` (with clean `free_texture`/`update_egui_texture_from_wgpu_texture` lifecycle on image change) or a `CallbackTrait` viewport pass.
  * Add GPU-accelerated export (with global-UV-aware tiled fallback for huge images) + headless `wgpu` test runner.
  * Write **CPU-vs-GPU parity tests** (dithering disabled, deterministic math) verifying pixel diffs stay within tolerance.
* **Done Criteria:** Real-time 60fps slider response at 1600px–2048px preview resolution on both Linux (Vulkan) and Windows (DX12).

### Phase 3: Detailed Filters & Custom UI Widgets
* **Deliverables:**
  * Interactive **Tone Curve** widget (monotone cubic spline, add/move/remove points, Master/R/G/B tabs, sRGB-domain evaluation with HDR extrapolation).
  * Non-blocking live **RGB/Luminance Histogram** + highlight/shadow clipping overlay + cursor probe + WB eyedropper.
  * **8-band HSL Mixer** and **3-way Color Grading Wheels** (Oklab/OkLCh + soft gamut mapping).
  * Spatial filters in WGSL + CPU (alpha-weighted): **Sharpening (Unsharp Mask)**, **Clarity** (mip-pyramid), **Gaussian Blur** (mip-pyramid), **Vignette**, and **Procedural Film Grain** (shared deterministic spatial hash, resolution-independent).
  * **Before/After** split-wipe & toggle + drag-batched **Undo/Redo** command history.
* **Done Criteria:** All Layer 1 filters work smoothly in preview and match full-res export.

### Phase 4: Composable Custom Stack & Preset Ecosystem
* **Deliverables:**
  * Dynamic `Vec<FilterNode>` custom stack (reorder, enable/disable, opacity, blend modes: Normal, Multiply, Screen, Overlay, Soft Light).
  * Custom nodes: Levels, 3x3/5x5 Convolution Kernels, Color Matrix, Duotone/Gradient Map, Bloom/Halation (mip-pyramid), Radial/Linear Gradient Masks, Film Grain, Posterize/Dither, 3D LUT.
  * `.cube` 3D LUT parser (import, sRGB-wrapped) and lattice baker (export with spatial-filter notice).
  * Left-dock Preset Browser with OS-standard storage, search/tags, selective apply checkboxes, inline embedded LUTs in shared presets, async `rayon` CPU miniature thumbnails, and starter presets in `assets/presets/`.
* **Done Criteria:** Can create complex multi-stage custom filters, save portable full or partial presets, browse with live thumbnails, and export `.cube` LUTs.

### Phase 5: Polish, `filter-cli`, & Cross-Platform Packaging
* **Deliverables:**
  * Add `filter-cli` for batch applying `.pfpreset.json` files to folders of images from the terminal.
  * Keyboard shortcuts, `eframe` window/settings persistence, optional `.pfedit.json` sidecars, export overwrite confirmation, better error toasts, memory/VRAM caps, and optional lossy WebP feature (`webp`).
  * GitHub Actions CI building release binaries for Linux (`x86_64-unknown-linux-gnu`) and Windows (`x86_64-pc-windows-msvc`), running `cargo test` (including headless GPU/CPU parity tests).

### Phase 6 (Future): Camera RAW & Shared Crate Extraction
* **Deliverables:**
  * Implement `RawDecoder` behind `feature = "raw"` in `filter-io`.
  * Optionally extract `egui-image-viewport` / `filter-core` hooks for reuse in procedural image generation projects.

---

## 10. Verification & Testing Strategy

* **Color Math & Safety Unit Tests:** Verify `srgb_to_linear(linear_to_srgb(x))` round-trips, `moxcms` ICC profile conversion (e.g., Display P3 -> Linear sRGB), Oklab/OkLCh signed `cbrt` with negative inputs (no `NaN`s), and monotone spline bounds/HDR extrapolation.
* **CPU / GPU Parity Tests:** Run synthetic test gradients, negative/HDR patches, and alpha images through both `filter-core::cpu_eval` and headless `filter-gpu` (with dithering off and deterministic grain hash) for every filter and assert both linear `f32` tolerance and quantized sRGB `max_abs_diff < 2.0 / 255.0` (watching the `12.92x` near-black sRGB toe amplification under `Rgba16Float`).
* **Metadata & Preset Compatibility Tests:**
  * Verify EXIF orientation is applied on load and reset (`Orientation = 1`) on export so reloading never double-rotates.
  * Keep snapshot JSON fixtures from `schema_version: 1` onward and test partial preset deserialization (distinguishing `None`/omitted sections from explicit `0.0` resets).
* **Edge-Case Checks:**
  * Images with alpha channels (no dark edge halos in spatial filters; clean premultiplied display vs. straight file export).
  * Grayscale, 16-bit PNGs, wide-gamut ICC JPEGs, and graceful error handling on unsupported color types (e.g., CMYK if not converted by decoder/CMS).
  * Very wide panoramas or `>8K` images triggering global-UV-aware tiled export without GPU OOM.
  * HiDPI display scaling (`pixels_per_point`) and non-ASCII file paths on Windows & Linux.
