# Live preview histogram

The adjustment panel displays four 256-bin curves: encoded sRGB red, green and blue,
and encoded Rec.709 luminance (`Y`). The common vertical scale is linear and normalized
to the largest bin among the four channels. Hover the panel for channel clipping and
sample counts.

## Measurement

- The input is the same immutable, premultiplied `Rgba16Float` proxy uploaded for the
  viewport, with a longest edge of at most 2048 pixels. All proxy texels are measured;
  pan, zoom, full-resolution tile refinement and viewport clipping do not affect it.
- Each texel is unpremultiplied with the viewport's alpha epsilon, then evaluated with
  the same validated `BasicAdjustments` snapshot and canonical WGSL functions used by
  preview/export. The viewport and histogram share their output-transform WGSL helpers.
- Linear Rec.2020 converts to linear sRGB, then clips to `[0,1]`, matching the current
  display transform. Soft gamut compression and alternate output spaces remain pending.
- RGB bins use sRGB transfer encoding. `Y` is `0.2126 R + 0.7152 G + 0.0722 B` from
  the clipped **linear** sRGB components, followed by the same sRGB transfer encoding.
  It is not an average of the encoded RGB channels.
- Bin `i` covers `[i/256,(i+1)/256)`; encoded white is included in bin 255. Measurements
  precede 8-bit quantization/dithering and checkerboard compositing. JPEG background
  flattening, export resizing and export bit depth do not affect this preview analysis.

Fully transparent texels (`alpha <= 0`) are excluded even if their hidden color is
invalid. Every positive-alpha texel counts once, including partially transparent
texels; counts are not weighted by alpha. Active nonfinite input/output samples are
excluded and reported as invalid. The CPU reference reproduces the premultiplied
half-float upload and unpremultiplication before evaluating the pointwise stages.

Low/high clipping counts are tracked for each RGB channel and `Y` at the exact
display endpoints, including originally black/white samples. The summary counts
preview pixels with **any** RGB channel at black or white; one pixel may appear in
both summaries. Luminance clipping instead describes the endpoints of the displayed
luminance. Saturated colors can therefore clip a channel without clipping luminance.
Counts apply to the proxy and are an approximation of full-resolution image statistics.

## Asynchronous updates

`filter-gpu::GpuHistogram` computes in 16×16 workgroups. Each group accumulates 4×256
local atomic bins plus sample/clipping counters, then merges nonzero counts into a
global buffer. Partial edge groups participate in both workgroup barriers. The source
texture, compute pipeline, parameters, global counters and source bind group are reused.

Two 4160-byte staging buffers receive `map_async` readbacks. Only counters are copied;
there is no full-image readback or second image texture. A shared 4160-byte storage
buffer is cleared before every dispatch. Commands clear, compute and copy in queue
order, keeping each submitted adjustment snapshot independent. A busy slot is never
overwritten; submission returns `false` when both are occupied. Collection validates
bin totals and source pixel counts, drops the mapped view, then unmaps before reuse.
Mapping errors or disconnected callbacks retire the affected buffer without calling
`unmap` on an unmapped buffer. Its next submission allocates a fresh staging buffer,
so retry does not reuse uncertain mapping state or raise an unmapped-buffer validation error.

The app coalesces edit changes to its latest desired generation and rejects old
completions and errors. Successful image replacement invalidates old generations even
if the new image uses identical settings. A change to the uniform rows, the baked tone curves (compared by content) or the LUT starts a new generation, so a curve-point drag re-measures. Pan/zoom and idle repaints submit no work.
The most recent completed curve stays dimmed and marked **Updating…** during an edit;
its counts are hidden until the current generation completes. Image replacement or
unsupported adjustments clear the display. **Retry histogram** retries a failed current
generation without decoding the source again.

The ordinary UI frame performs only `PollType::Poll` and channel `try_recv`; neither
submission nor collection waits for the GPU. Pending readback requests a repaint after
16ms. This targets a subsequent-frame update; it is not a guaranteed latency on every
adapter, especially during an export or software rendering. Native desktop performance
and Windows/DX12 behavior must be verified separately from local Linux tests.

## Clipping overlay

The viewport can flag clipped pixels. **Shadow clipping** paints opaque pure blue
and **Highlight clipping** opaque pure red over the canvas; **J** toggles both (if
either is off both turn on, otherwise both turn off) unless a text field has focus.
A held key toggles once; key-repeat events do not toggle again.

- A pixel is flagged when the unclipped linear sRGB output of the basic-adjustment
  pipeline would display as 8-bit sRGB 255 in any channel (highlights: linear
  value at or above 0.99554525, the decode of 254.5/255) or as 0 in any channel
  (shadows: below 0.5/255/12.92 ≈ 1.5176e-4). This is the histogram's any-RGB test
  widened by half a display step at each end. The margin keeps f32 interpolation
  noise (around 1e-6 of a neighboring texel) from flipping exact endpoint tests
  between full-resolution tiles and the untiled path. The histogram keeps its exact
  endpoints (`shadow_clipped`/`highlight_clipped` in `output.wgsl`), so a pixel
  within half a step of black or white can be flagged without being counted.
- Flagged fragments visible at 8 bits (alpha at least 0.5/255) show the overlay color instead of the
  checkerboard composite, in display color with no alpha mixing. Where both
  conditions hold and both overlays are on, highlights win. Fragments with lower
  alpha keep the checkerboard. Nonfinite output is not flagged, as the histogram counts it
  as invalid instead.
- It is evaluated on whatever the fragment shows: the unedited source on the
  before side of a before/after split and while Space is held is flagged by the same
  rule, from its own values.
- Sampling is unchanged. The overlay applies its tests per fragment to the
  viewport's sampled values (bilinearly interpolated from the proxy or source
  tiles, then unpremultiplied), not to the exact pixel set the histogram counts.
  At zoom levels other than 1:1 the boundary of a flagged area follows the
  filtered image, so the overlay and histogram agree only approximately there and
  at alpha edges.
- The two flags live in the renderer's shared viewport uniform
  (`ExposureRenderer::set_clipping_overlay`), so the proxy and full-resolution tiles
  agree with no extra pass, texture or per-tile uniform.
- It is view state only: not in `EditState`, undo history or presets, kept across
  image loads in the session, and not restored on restart. Histogram, cursor probe
  and export do not change.

## Verification

```sh
cargo test --workspace --locked
cargo test --workspace --locked -- --ignored --skip large_image_exports --test-threads=1 --nocapture
```

Core tests specify bin boundaries, encoded luminance, clipping, partial/zero alpha,
invalid samples and readback consistency. GPU tests compare complete distributions
against the CPU reference for fixed input patches, including mixed adjustments and
HDR/negative values. They exercise simultaneous snapshots, full slot backpressure,
source replacement while pending, reuse after unmapping, counter clearing, invalid
samples, edge workgroups and device/source limits. App tests cover stale generations,
unsupported edits, retry and coalescing; a software-GPU headless egui test also checks
idle behavior and panel rendering. These checks do not exercise a native desktop window.
An additional software-GPU regression injects mapping failure and callback disconnection,
checks that collection produces no wgpu validation errors, and verifies successful retry
with a replacement buffer.

Clipping overlay checks (software GPU) render known white, black, mid-gray,
both-way-clipped, single-channel highlight/shadow, transparent and half-transparent
pixels with every flag combination through tiled and untiled frames on both target
paths, including a multi-tile image whose tiled and untiled renders must match byte
for byte, and a +3 EV edit that pushes mid-gray into highlight clipping. They check exact overlay colors, byte-identical output with the flags off and
the before side of a split. A headless app test covers the checkboxes, J, text-field
gating and persistence across image loads. Native desktop behavior is unverified.
