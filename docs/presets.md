# Edit and preset model

`filter-core` provides the initial Phase 1 model without GUI or GPU dependencies.
The desktop app stores its adjustments in `EditState`; preview and export render
calibration, basic tone, vibrance and saturation. Other stored parameters and
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
example Tone containing clarity, Curves, or Color containing HSL/grading).
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
present sections and retains its ID, author and tags. Saving captures every
field of each selected section, including neutral values; it does not reproduce
the original sparse field patch. The custom stack is captured as a replacement
snapshot, even if the imported preset appended nodes. Node IDs refresh on apply
under the merge contract below. Unknown operation payloads survive this workflow;
unselected sections are intentionally omitted.

`filter_io::preset::read` bounds both the file-size check and actual read to
16 MiB before parsing. `write` validates and checks serialized size before
writing a sibling temporary file, flushes it, and atomically publishes it.
Validation or writing failure leaves an existing destination intact. Temporary
files are removed on failure. File work and native dialogs run on workers; the
UI admits one dialog/preset file operation at a time. Preset files use private
temporary-file permissions. The library caller owns overwrite confirmation.

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
space. `schema_version: 1` describes the preset layout. Both are required in
preset JSON; edits require their process version. Missing, zero, and unsupported
versions are errors. Future layout migrations belong at the raw JSON boundary;
there are no earlier schemas to migrate today. A preset never changes an edit's
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
| Finishing | Vignette and grain fields overwrite individually. |
| Custom stack | `Replace` (default) replaces the list; `Append` appends. Every incoming node receives a fresh UUID. |

A missing custom stack leaves it untouched; an empty list with `Replace` clears
it. Existing IDs survive an append. Crop is stored as normalized
`[left, top, right, bottom]`, is always `[0, 0, 1, 1]` in v1, and is not changed by
presets.

Curves contain 2–1024 finite points with strictly increasing x coordinates and
x endpoints 0 and 1. Their y coordinates can extend beyond [0, 1]. Exposure must
be within −5 to +5 EV. Temperature/tint, brightness, contrast, highlights/shadows, whites/blacks,
vibrance and saturation must be within −1 to +1. Other scalar parameters must be
finite; their ranges and algorithms await subsequent engine work.

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

LUT payload/overlay schemas, additional node types, a full stack UI,
preset browser/thumbnails, and sidecar file I/O remain subsequent tasks. Newer unknown
operations can already survive a load/save round trip in the current schema.

Fixed v1 JSON fixtures and regression tests live in
`crates/filter-core/tests/fixtures` and `crates/filter-core/tests/preset_model.rs`.
