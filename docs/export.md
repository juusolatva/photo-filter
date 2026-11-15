# PNG export and metadata

Desktop export renders the captured source and basic adjustments into a full-resolution,
16-bit sRGB PNG with straight alpha and an embedded output sRGB ICC profile. It streams
GPU tiles into bounded row bands and publishes the completed file atomically. Editing or
opening another image does not change the source, settings or metadata of an active export.

Available source EXIF bytes are copied into the PNG eXIf chunk. A recognized IFD0
Orientation tag is reset to `1` using `image::metadata::Orientation::remove_from_exif_chunk`:
the decoder has already applied the rotation or reflection to the pixels. Source metadata
is never modified. Both little-endian and big-endian EXIF are supported. Camera tags,
timestamps, unknown tags and their offsets remain intact because no metadata is repacked.

This is orientation normalization, not a general metadata rewrite. Other source
descriptions, including dimension tags, EXIF color-space tags and embedded thumbnails,
remain unchanged and may describe the original image. The PNG dimensions and output ICC
profile describe the exported pixels. Missing, invalid or unrecognized orientation is
left unchanged, matching the decoder's interpretation. No EXIF is synthesized when none
is available; the current TIFF decoder does not expose source EXIF. XMP and other metadata
outside the retained EXIF payload are not copied.

The `filter-io` API `write_srgb16_png_with_exif` accepts optional raw EXIF from
`image::ImageDecoder` and pixels that are already oriented. EXIF is limited to 16 MiB,
matching the import limit; oversized metadata fails before writing the PNG header.
Empty EXIF is omitted. Nonempty EXIF is checked with the strict `kamadak-exif`
reader before any PNG bytes are written. Malformed headers, directories or value
offsets fail export with an `Invalid source EXIF metadata` error, leaving an existing
destination untouched. Structurally valid metadata with missing or unrecognized
orientation is retained.
`write_srgb16_png` remains available for callers that do not need EXIF.

Workspace tests export and reload PNG/JPEG/WebP sources with each of the eight EXIF
orientations and both byte orders. They check pixel positions and values, dimensions,
orientation normalization, output ICC, camera metadata preservation and source immutability.
The software-GPU basic-adjustment export regression also verifies EXIF in the tiled path.
