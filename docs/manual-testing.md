# Linux and Windows test checklist

Run this checklist separately on **Linux** and **Windows**. Tick only after checking
the expected result. Record failures with the OS, GPU, display scale and steps.

Use a release build for large photos. Have a normal photo, a rotated EXIF photo,
a transparent PNG, a sharp pixel grid and a photo larger than 36 MP ready.

## Open and view

- [ ] Open JPEG, PNG, WebP and TIFF using **Open image**, **Ctrl+O** and drag/drop; each displays correctly.
- [ ] Open a rotated EXIF photo; it appears upright with the correct dimensions.
- [ ] Open a file from a path with spaces and non-ASCII characters; it loads normally.
- [ ] Try a corrupt image, then a valid image; an error appears and the app recovers.
- [ ] Switch images rapidly; the last requested image wins without old content reappearing.
- [ ] Use **Fit**, **100%**, pan and scroll zoom; framing and zoom around the pointer stay correct.
- [ ] At 100% or above, pan across a large image; detail refines without tile seams or dark transparency edges.

## Adjustments and histogram

- [ ] Move every basic slider; the preview changes. Click **Reset adjustments**; the preview returns to neutral.
- [ ] Drag sliders rapidly; preview and histogram settle on the final settings without freezing.
- [ ] Pan and zoom; histogram stays unchanged. Edit exposure; curves and clipping counts update.
- [ ] Hover the histogram; counts appear. During updates, old curves dim and old counts are hidden.

## Undo and redo

- [ ] Drag a slider in several motions without releasing; one **Undo** (or **Ctrl+Z**) returns to the value before the press.
- [ ] Type a slider value and press Enter; one undo restores the previous value. While typing, **Ctrl+Z** edits the text instead.
- [ ] Hold an arrow key on a focused slider, then release; the whole held change undoes as one step.
- [ ] Apply a preset and click **Reset adjustments**; each undoes separately. **Redo**, **Ctrl+Shift+Z** and **Ctrl+Y** step forward again.
- [ ] Undo, then move a slider; **Redo** becomes unavailable. Open another image; Undo and Redo start unavailable.
- [ ] Undo/redo during an export; the export keeps the settings it captured.

## Cursor color probe

The readout is **Cursor color**, below the histogram. It measures one original
source pixel, so its color can differ from the blended screen color at Fit or fractional zoom.

- [ ] Hover a pixel grid at 100% or above; zero-based coordinates and RGB/hex follow the pointed source pixel.
- [ ] Repeat at **Fit**, fractional zoom and after panning; coordinates still identify the same source pixels.
- [ ] Move into empty canvas, outside the image or exactly onto its right/bottom boundary; the reading clears.
- [ ] Change exposure/color, reset and apply a preset; return to the same pixel and confirm RGB/hex and Oklab reflect the current edit.
- [ ] Hover opaque, partly transparent and fully transparent pixels; alpha is separate, RGB excludes the checkerboard, and zero alpha shows **Transparent**.
- [ ] Move quickly across pixels, then stop; the final reading matches the final pointer position. Switch images; old readings never return.
- [ ] Repeat while exporting and at 125%/150% display scaling; mapping stays correct and interaction stays responsive.

## Presets

- [ ] Save a preset, reset, load and apply it; the selected adjustments return. Loading alone leaves edits unchanged.
- [ ] Apply only one preset section; unchecked sections keep their current values.
- [ ] Load invalid JSON, then try applying unsupported active preset effects; both report errors and leave edits intact.
- [ ] Cancel load/save dialogs and decline overwriting an existing preset; files and edits stay intact.

## Export

- [ ] Export and reopen PNG and TIFF at both 8/16-bit, JPEG and lossless WebP; dimensions, orientation and edits are correct.
- [ ] Export transparent areas; PNG/WebP/TIFF retain alpha, and JPEG uses the selected background without dark edge halos.
- [ ] Try **Full**, **Long edge** and **Percentage**; reopened dimensions match the preview and aspect ratio is preserved.
- [ ] Change JPEG quality and toggle 8-bit dithering; controls work and files reopen.
- [ ] Select TIFF; the export controls clearly state that camera metadata is omitted.
- [ ] Start a large export, then edit or open another image; the UI remains usable and the output retains the captured image/settings.
- [ ] Cancel an export replacing a disposable existing file; its original contents survive. Allow WebP compression to finish before expecting cancellation to complete.
- [ ] Try exporting to an unwritable location; an error appears without damaging an existing destination.
- [ ] Export a >36 MP photo at full size and reduced size; both finish and reopen without missing bands or tiles.

## Platform checks

- [ ] **Linux:** native open/save/overwrite dialogs work through the desktop portal; repeat view/probe checks on X11 and Wayland where available.
- [ ] **Windows:** native open/save/overwrite dialogs work; repeat view/probe checks at 100%, 125% and 150% display scaling.
- [ ] **Both:** resize the window and move between monitors with different scaling; 100% stays one source pixel per physical display pixel and the probe stays aligned.
- [ ] **Both:** launch with `--software-gpu` (Linux: lavapipe/Vulkan; Windows: WARP/DX12); repeat opening, zooming, editing, probing and one export.

GPU mapping failures, invalid pixels and retry recovery are covered by automated
fault-injection tests; ordinary desktop use cannot reliably trigger them. This
checklist covers native interaction and responsiveness that headless tests cannot establish.
