# Edit and preset model

`filter-core` provides the initial Phase 1 model without GUI or GPU dependencies.
The desktop app stores its adjustments in `EditState`; preview and export render
calibration, the channel mixer and monochrome, basic tone, curves, the HSL mixer, vibrance, saturation, color grading and a Finishing `.cube` LUT. Other stored parameters and
custom nodes await their engine stages. The app supports preset file save/load and
selective application. See [the pointwise renderer](pointwise.md).

## Desktop workflow

Open an image, then expand **Presets** in the adjustment sidebar. **Load preset…**
opens a native JSON file dialog; dropping a `.pfpreset.json` (or other `.json`)
file onto the app loads it too. Loading stages the preset without changing the
image. Check the sections to apply and select **Apply selected sections**.
Only sections present in the file are selectable. Unchecked sections retain
their current values, including explicit nonzero settings.

The current renderer rejects selected active effects it cannot render, leaving
the edit and preview unchanged. The error names the sections to deselect (for
example Tone containing clarity, or Color containing defringe).
Opaque unknown operations are preserved in the applied custom stack, skipped by
the renderer, and identified in the loaded-preset panel. Schema/process version
errors and invalid parameters are rejected at load time, even in sections that
would not be selected. A successful image replacement clears staged presets;
results from loads started for an older image are discarded.

Enter a preset name, choose **Sections to save**, and select **Save preset…**.
The default filename is `edited.pfpreset.json`; the native dialog confirms
replacement of an existing destination. Saving captures the current edit when
the button is pressed, so subsequent editing or image replacement does not
change the saved file. After applying a preset, saving initially selects its
present sections and retains its author and tags. Each save gets a fresh preset
ID. Saving captures every
field of each selected section, including neutral values; it does not reproduce
the original sparse field patch. The custom stack is captured as a replacement
snapshot, even if the imported preset appended nodes. Node IDs refresh on apply
under the merge contract below. Unknown operation payloads survive this workflow;
unselected sections are intentionally omitted.

`filter_io::preset::read` accepts regular files (including links to regular
files), rejects directories/devices/pipes, and bounds both the file-size check and actual read to
16 MiB before parsing. `write` validates and checks serialized size before
writing a sibling temporary file, flushes it, and atomically publishes it.
Validation or writing failure leaves an existing destination intact. Temporary
files are removed on failure. The containing directory is not synced after the
rename, so publication does not guarantee durability across system crashes or
power loss. File work and native dialogs run on workers; the
UI admits one dialog/preset file operation at a time. New files follow normal
creation permissions/umask; replacements preserve existing Unix permission bits.
Atomic replacement does not preserve ownership, filesystem ACLs or extended
attributes; use a separate destination when those access rules must be retained.
Saving through an existing symbolic link updates its resolved target and keeps
the link; a broken link is rejected without replacing it.
The library caller owns overwrite confirmation.

The library continues to support stored effects that the current desktop
renderer cannot execute. The OS preset library, search/tags editor, thumbnails,
starter assets, and sidecars remain future work.

## API

```rust
use filter_core::{edit::EditState, preset::{Preset, Sections}};

fn main() -> Result<(), filter_core::edit::ModelError> {
    let mut edit = EditState::default();
    edit.calibration.exposure = 0.75;
    let calibration = Sections { calibration: true, ..Sections::default() };
    let preset = Preset::capture("Exposure +0.75", &edit, &calibration)?;
    let json = preset.to_json()?;
    let loaded = Preset::from_json(&json)?;
    loaded.apply_to(&mut edit, &Sections::ALL)?;
    Ok(())
}
```

`Preset::from_json` is the preset-loading boundary: it checks the raw schema,
then deserializes and validates the patch. `Preset::to_json` validates before
writing JSON. These methods do not access files. Serde deserialization of an
`EditState` also validates before returning it, including unique custom-node IDs.
Use `EditState::validate()` after constructing or modifying an edit in Rust.

Preset imports are capped at 16 MiB before parsing, including opaque operation
payloads. Stacks are capped at 256 nodes; append/replace checks run before cloning
the edit. These are library limits; file-reading callers must also bound the
bytes they read before building an input string.

`process_version: 1` pins algorithm behavior and the linear Rec.2020 working
space. `schema_version` describes the preset layout: schemas 1 and 2 exist (there
is no v0). Version 2 is written only when a LUT is embedded; `migrate()` has a
v1 to v2 step and rejects a v1 file that carries a cube. Both versions are
required in preset JSON; edits require their process version. Missing, zero, and
unsupported versions are errors. Layout migrations run at the raw JSON boundary. A preset never changes an edit's
process version. Algorithm upgrades require an explicit future workflow.

## Merge contract

All edit fields have concrete defaults. `PresetPatch` uses optional sections and
fields. Omission or JSON `null` means leave the destination untouched; `0.0` and
`false` are explicit replacements. Unknown fields are ignored. Unknown operation
payloads are preserved as semantic JSON, rather than as original text formatting.

`Sections::default()` selects nothing; `Sections::ALL` selects all seven sections.
The same mask controls capture and application. Applying a patch clones the
current edit and validates the result before replacing it, so failure leaves the
edit untouched. Direct patch application ignores unselected sections; loading a
preset validates every section in the file.

| Section | Merge rule |
| --- | --- |
| Calibration | Exposure, relative temperature and tint overwrite individually. |
| Tone | Brightness, contrast, highlights, shadows, whites, blacks and clarity overwrite individually. |
| Curves | Each present master/red/green/blue curve replaces that channel's points. |
| Color | Vibrance, saturation, channel mixer, monochrome, defringe and grading balance overwrite individually; each present HSL band or grading wheel replaces that entry. |
| Detail | Lateral CA pair, noise reduction and sharpening fields overwrite individually. |
| Finishing | LUT, LUT strength, vignette and grain fields overwrite individually. A missing `lut` key leaves the current LUT untouched; `{"kind":"none"}` clears it. |
| Custom stack | `Replace` (default) replaces the list; `Append` appends. Every incoming node receives a fresh UUID. |

A missing custom stack leaves it untouched; an empty list with `Replace` clears
it. Existing IDs survive an append. Crop is stored as normalized
`[left, top, right, bottom]`, is always `[0, 0, 1, 1]` in v1, and is not changed by
presets.

Curves contain 2–1024 finite points with strictly increasing x coordinates and
x endpoints 0 and 1. Their y coordinates can extend beyond [0, 1], within −1 to 2
(encoded sRGB units); see [tone curves](pointwise.md#tone-curves). Exposure must
be within −5 to +5 EV. Temperature/tint, brightness, contrast, highlights/shadows, whites/blacks,
vibrance and saturation must be within −1 to +1. HSL band values, grading lightness and
grading balance are also −1 to +1 (see [the HSL mixer](pointwise.md#hsl-mixer) and
[color grading](pointwise.md#color-grading)). Grading hue (turns of OkLCh hue) and chroma
are 0 to +1. Channel mixer coefficients are −2 to +2; rows are output channels in linear
sRGB primaries, and monochrome outputs the luminance of the mixed color (see
[the channel mixer](pointwise.md#channel-mixer-and-monochrome)). Defringe must be finite
and is not yet rendered; nonzero values are rejected at render time. Out-of-range values
are rejected, never rescaled.

Color JSON flattens scalar fields inside `color`. `hsl`, when present, contains
eight entries in red/orange/yellow/green/aqua/blue/purple/magenta order. `grading`
contains three entries in shadows/midtones/highlights order. Use `null` for entries
to retain. A present entry replaces the entire band/wheel, with omitted components
defaulting to zero. For example:

```json
{
  "color": {
    "vibrance": 0.2,
    "hsl": [null, {"hue": 0.1}, null, null, null, null, null, null],
    "grading": [null, {"chroma": 0.1}, null]
  }
}
```

The channel mixer is stored as `channel_mixer` (three rows of three coefficients) with
`monochrome`. A black-and-white look sets all three rows equal to weights that sum to one
and `monochrome: true`, for example an orange filter:

```json
{"color": {"monochrome": true, "channel_mixer": [[0.62, 0.35, 0.03], [0.62, 0.35, 0.03], [0.62, 0.35, 0.03]]}}
```

Setting `monochrome: false` afterwards without restoring `channel_mixer` leaves a gray
image, because the equal rows remain; the desktop UI resets the matrix to the identity when
monochrome is unchecked with equal rows. `Reset channel mixer` restores the identity and turns
monochrome off.

## Finishing LUT

Desktop workflow: under **Look (LUT)**, **Load .cube…** opens a native file dialog (one file
operation at a time, like presets); the panel then shows the title (or file name), the size as
`N³` (plus the domain when it is not 0..1), a **Strength %** slider, **Replace…** and
**Remove**. Strength is 0 to 100%; double-click resets to 100%. Load, a strength drag and
Remove are each one undo step. Remove also resets strength to 100%. Loading a LUT identical
to the current one (same lattice, domain and title) changes nothing and records no step; a
first LUT starts at 100% and a replacement keeps the current strength. A failed load shows
the parser message (with the 1-based line number, never the path) and leaves the edit,
preview and history untouched; a result for a previous image is discarded silently. Drag and
drop of `.cube` files is not implemented. The file limit is 24 MiB, sizes 2 to 65.
See [the LUT stage](pointwise.md#cube-lut-look) for the math.

The model is `Finishing.lut` (a `LutSlot`, never `null`) and `Finishing.lut_strength`
(finite, 0 to 1; default 1, ignored without a LUT):

```json
"finishing": {
  "lut_strength": 1.0,
  "lut": {
    "kind": "cube", "title": "Warm Portrait Negative", "size": 33,
    "domain_min": [0, 0, 0], "domain_max": [1, 1, 1],
    "encoding": "f16-shuffle-zlib-v1", "data": "<base64>"
  }
}
```

or `"lut": {"kind": "none"}`. Unknown `kind`, unknown fields and an unknown `encoding` are
errors. A `none` is an explicit value, so saving the Finishing section with no LUT writes
`{"kind":"none"}` and applying that section clears the current LUT. A preset with no `lut`
key (every file written before this feature) leaves the current LUT untouched. Saving without
the Finishing section omits both fields.

Embedding: the lattice is little-endian f16 (6 bytes per node, R fastest), byte-shuffled into a
low-byte plane then a high-byte plane, zlib-compressed (level 6) and written as standard
padded base64 with no line breaks. Encoder output bytes are not stable across zlib backend
versions; tests compare decoded lattices only. No hash is stored.

Decode limits, in order: the wire object is validated first (size 2 to 65, finite domain with
`|v| <= 8` and width of at least 1e-6, title of at most 128 characters with control
characters removed, known `encoding`); the base64 length is checked against a per-size bound
(`4 * ceil((raw + raw/1000 + 64) / 3)` for `raw = 6 N³`, at most 3 MiB) before decoding;
base64 is strict (alphabet, padding, no whitespace, canonical trailing bits); inflation is
capped at `raw + 1` bytes whatever the compression ratio, and the result must be exactly `raw`
bytes with nothing after the zlib stream; every value must be a finite f16 with `|v| <= 8`.
Any failure rejects the whole preset at load, naming the LUT. The whole file stays under the
16 MiB limit. A duplicate `lut` key is rejected.

Schema: a preset is written as `schema_version` 2 only when its Finishing patch holds a LUT,
and as version 1 otherwise, so LUT-free presets still load in older builds. Readers accept 1
and 2; a v1 file carrying a `cube` is rejected. An older build that only knows version 1
rejects a v2 file with "unsupported preset schema version 2" instead of silently dropping
the look. `EditState` serialization is otherwise unversioned and only presets write it.
Applying a preset keeps the current `Arc` for an identical LUT, so it adds no upload.
A Finishing section holding only a LUT is renderable; vignette or grain still make the section
unsupported.

Each history step counts its LUT at about `6 N³` bytes without deduplicating shared payloads
(about 216 KB at 33³, 1.65 MB at 65³), so a 65³ LUT leaves roughly 38 undo steps within the
64 MiB budget. Evicting steps early is the only effect.

## Custom nodes and remaining work

Nodes carry a UUID, name, enabled flag, insertion position, blend mode, opacity
and operation. Initial typed schemas are `Levels`, `ColorMatrix3x3`, and
`FilmGrain`; scale metadata identifies pointwise and pixel-scale operations.
These schemas do not implement rendering. Known operations with malformed
parameters are errors. Other operation types become `FilterOp::Unknown` and
retain the complete operation payload, including nested data and integer values.
Their stored enabled flag is retained for future applications, but
`FilterNode::effective_enabled()` returns false so an executor can skip them.
Node opacity must be finite and within [0, 1].

The Layer 2 `Lut3D` node, additional node types, a full stack UI,
preset browser/thumbnails, and sidecar file I/O remain subsequent tasks. Newer unknown
operations can already survive a load/save round trip in the current schema.

Fixed JSON fixtures (v1, and decode fixtures for the v2 LUT cases) and regression tests live in
`crates/filter-core/tests/fixtures` and `crates/filter-core/tests/preset_model.rs`.
