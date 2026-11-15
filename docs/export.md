# Image export and metadata

Desktop export renders the captured source and basic adjustments at full resolution.
Choose PNG for 16-bit sRGB RGBA with straight alpha, or JPEG for 8-bit sRGB RGB with
quality 1–100 (default 90). Both embed an output sRGB ICC profile and retain validated
source EXIF. The chosen format must match the filename: `.png` or `.jpg`/`.jpeg`,
case insensitive. The native save dialog confirms existing destinations.

JPEG composites straight alpha against a user-selectable sRGB background (default white).
The background is converted to linear sRGB and composited with the rendered linear RGB
before output transfer and quantization; a half-transparent black pixel over white is
about 188/255. Zero-alpha pixels ignore hidden RGB. Colors outside the final
sRGB gamut are clipped. JPEG uses seeded triangular dithering (±1 quantization step)
by default; the checkbox disables it. Noise depends on pixel/channel index and seed,
so changing tile or band boundaries does not change it. PNG retains 16-bit quantization
without dithering. Gamut compression and other output color spaces remain pending.

Both formats stream GPU tiles into bounded row bands and publish the completed file
atomically. Editing, changing export controls or opening another image does not change
the source, settings or metadata of an active export. Progress reserves a final step for
encoding and publication. Cancellation is checked between tiles, during GPU readback,
and on buffered file writes during encoding. Failed/cancelled work leaves the destination
intact and removes the temporary file. New files use normal creation permissions;
replacements preserve existing Unix permission bits.

PNG encoding streams without a full-image CPU output buffer. The explicit `image`
JPEG encoder uses an RGB8 CPU buffer (3 bytes per pixel) in addition to the current
row band, tile buffers and immutable half-float source. This output buffer is capped
at 512 MiB with fallible allocation; neither format allocates a full-image GPU output
texture or readback buffer. JPEG dimensions must each be between 1 and 65535.

Available source EXIF bytes are copied into the PNG eXIf chunk or JPEG APP1 segment. A recognized IFD0
Orientation tag is reset to `1` using `image::metadata::Orientation::remove_from_exif_chunk`:
the decoder has already applied the rotation or reflection to the pixels. Source metadata
is never modified. Both little-endian and big-endian EXIF are supported. Camera tags,
timestamps, unknown tags and their offsets remain intact because no metadata is repacked.

This is orientation normalization, not a general metadata rewrite. Other source
descriptions, including dimension tags, EXIF color-space tags and embedded thumbnails,
remain unchanged and may describe the original image. The encoded dimensions and output ICC
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

The `write_srgb8_jpeg` API accepts the same band layout and optional source EXIF with
`JpegOptions` for quality, background and optional dither seed. The APP1 segment's 65535-byte
length includes two length bytes and the six-byte `Exif\0\0` prefix, allowing **65527 raw
EXIF bytes**. Larger metadata fails before consuming any bands or writing output; it is
never silently truncated or stripped. PNG retains its 16 MiB EXIF limit. Empty EXIF is
omitted in both formats, and the shared strict validation/orientation normalization has
the same preservation limitations described above. ICC describes the output pixels;
other source EXIF color-space/dimension/thumbnail descriptions remain unchanged.

JPEG tests cover linear-light alpha flattening over a colored background, hidden color,
quality-dependent compression, seeded/band-independent dithering, ICC and EXIF roundtrips
in both byte orders and all eight orientation values, invalid input and APP1 limits,
producer failures and cancellation during encoding. Software-GPU tests compare PNG/JPEG
exports against captured CPU adjustments and check every output pixel of a >36MP image
with a 2048px device texture limit (within JPEG's lossy tolerance).
