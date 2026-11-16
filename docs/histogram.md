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
if the new image uses identical settings. Pan/zoom and idle repaints submit no work.
The most recent completed curve stays dimmed and marked **Updating…** during an edit;
its counts are hidden until the current generation completes. Image replacement or
unsupported adjustments clear the display. **Retry histogram** retries a failed current
generation without decoding the source again.

The ordinary UI frame performs only `PollType::Poll` and channel `try_recv`; neither
submission nor collection waits for the GPU. Pending readback requests a repaint after
16ms. This targets a subsequent-frame update; it is not a guaranteed latency on every
adapter, especially during an export or software rendering. Native desktop performance
and Windows/DX12 behavior must be verified separately from local Linux tests.

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
