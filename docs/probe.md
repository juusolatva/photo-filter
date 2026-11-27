# Cursor color probe

For desktop testing, use the [Linux and Windows checklist](manual-testing.md#cursor-color-probe).
The details below describe the measurement and implementation contract.

Hover over the visible image to read the containing original source pixel in
**Cursor color**, below the histogram. Coordinates are zero-based in the oriented
source image. The probe follows the canvas's image rectangle through pan, Fit,
100%, fractional zoom and display scaling; letterboxed space and the right/bottom
image edges have no sample.

The measurement always uses one full-resolution source pixel. At Fit zoom this
can differ from the area-filtered proxy; at fractional zoom it can differ from
bilinear viewport interpolation. It does not average a screen footprint or patch.
The original straight-alpha half-float Rec.2020 pixel is converted to f32 and sent
through canonical basic-adjustment WGSL and the shared viewport/histogram linear
sRGB output transform. No full-image GPU texture, source tile upload or image
readback is needed. This path currently supports the same basic adjustments as
the viewer. It must be extended with the renderer when other effects are added.

The output is clipped to sRGB, with no gamut compression, compositing, alpha
flattening or dithering. RGB and `#RRGGBB` show nearest-rounding 8-bit encoded
sRGB. Oklab is computed from the same clipped linear sRGB output (L in [0, 1]);
it does not describe unclipped working-space color. Alpha is shown separately as
a percentage and does not attenuate RGB. Fully transparent pixels show
**Transparent** without exposing hidden RGB. Nonfinite visible source/output
samples show **Invalid pixel**.

One compute invocation writes a 32-byte result, copied to one of two 32-byte
asynchronous readback buffers. The input pixel and adjustment uniforms occupy
16 and 256 bytes, respectively. Buffers and bindings persist across jobs; the
payload is fixed regardless of image size. At most two jobs are pending, and
pointer/edit changes coalesce to the latest request. No blocking GPU poll or
readback runs on the UI thread. The app's ordinary nonblocking device poll drives
callbacks and pending jobs request a repaint.

Pointer-pixel, adjustment and image changes invalidate the displayed sample
before completed callbacks are collected. Older samples and errors cannot replace
the current target. Moving within the same source pixel or repainting an idle UI
submits no extra work. Leaving the image clears the readout. A current mapping
failure remains visible until **Retry color probe** or loading a new image;
retry re-enables sampling when the pointer returns to the image. Failed or
disconnected mapping callbacks retire their buffers without unmapping them;
the next submission allocates replacements. The histogram uses the same
`filter-gpu` readback ring, so this mapping/retirement rule has one implementation.

Automated checks cover output clipping/encoding/Oklab, transparency, invalid
samples, CPU/GPU agreement for basic adjustments, independent in-flight jobs,
slot reuse/backpressure, map failure/disconnection recovery, stale images/edits/
pointer positions, idle submissions, canvas mapping and panel rendering.
These checks use headless egui and a Vulkan software adapter. Native Linux/Windows
interaction and responsiveness still require desktop testing.

For manual testing:

- Hover at Fit and 100%, then pan and zoom around edges and letterboxed areas.
- Check that the coordinates and colors follow the original source pixels;
  use 100% or higher to inspect individual pixels.
- Adjust exposure/color, reset adjustments and apply presets, then hover again.
- Compare transparent and partially transparent areas with opaque colors.
- Load another image while sampling and check that no old readings reappear.
- Check the readout under fractional desktop scaling and while exporting.
