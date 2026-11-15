use anyhow::{Context, Result, ensure};
use filter_core::color::{linear_to_srgb, rec2020_to_linear_srgb, srgb_to_linear};
use half::f16;
use image::ImageEncoder;
use moxcms::ColorProfile;
use std::{borrow::Cow, io::Write};

/// JPEG options. Background channels are encoded sRGB; compositing uses linear light.
#[derive(Clone, Copy, Debug)]
pub struct JpegOptions {
    pub quality: u8,
    pub background: [u8; 3],
    /// Seeded triangular noise, in units of one 8-bit step. None disables dithering.
    pub dither_seed: Option<u64>,
}

impl Default for JpegOptions {
    fn default() -> Self {
        Self {
            quality: 90,
            background: [255; 3],
            dither_seed: Some(0),
        }
    }
}

/// PNG channel depth. Both modes preserve straight alpha.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PngBitDepth {
    Eight,
    #[default]
    Sixteen,
}

/// PNG output settings. Dithering affects only 8-bit RGB, never alpha.
#[derive(Clone, Copy, Debug)]
pub struct PngOptions {
    pub bit_depth: PngBitDepth,
    pub dither_seed: Option<u64>,
}

impl Default for PngOptions {
    fn default() -> Self {
        Self {
            bit_depth: PngBitDepth::Sixteen,
            dither_seed: Some(0),
        }
    }
}

/// Lossless WebP settings. Dithering affects 8-bit RGB, never straight alpha.
#[derive(Clone, Copy, Debug)]
pub struct WebPOptions {
    pub dither_seed: Option<u64>,
}

impl Default for WebPOptions {
    fn default() -> Self {
        Self {
            dither_seed: Some(0),
        }
    }
}

/// Encode oriented straight linear Rec.2020 bands as lossless 8-bit sRGB WebP.
/// Losslessness applies after output color conversion and quantization. Embeds
/// sRGB ICC and validated, orientation-normalized EXIF (at most 16 MiB).
/// Dimensions must be 1..=16384. Uses a fallibly allocated RGBA8 buffer capped
/// at 512 MiB, plus the current band and the encoder's internal buffers.
/// Caller owns atomic publication and cancellation of output writes.
pub fn write_srgb8_webp(
    output: impl Write,
    width: u32,
    height: u32,
    exif: Option<&[u8]>,
    options: WebPOptions,
    bands: impl IntoIterator<Item = Result<Vec<[f16; 4]>>>,
) -> Result<()> {
    write_srgb8_webp_cancellable(output, width, height, exif, options, bands, || false)
}

/// Like [`write_srgb8_webp`], checking cancellation before allocation, between
/// bands, at most every 4096 converted pixels, and before encoding. The encoder
/// compresses the entire frame internally before its first output write; that
/// pass cannot be interrupted. Caller must check writes and publication too.
pub fn write_srgb8_webp_cancellable(
    mut output: impl Write,
    width: u32,
    height: u32,
    exif: Option<&[u8]>,
    options: WebPOptions,
    bands: impl IntoIterator<Item = Result<Vec<[f16; 4]>>>,
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<()> {
    ensure!(!is_cancelled(), "Export cancelled");
    ensure!(
        (1..=16384).contains(&width) && (1..=16384).contains(&height),
        "WebP dimensions must be between 1 and 16384"
    );
    let count = u64::from(width) * u64::from(height);
    ensure!(
        count * 4 <= 512 * 1024 * 1024,
        "WebP RGBA buffer exceeds the 512 MiB limit"
    );
    let exif = normalized_exif(exif, crate::MAX_METADATA_BYTES)?;
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(count as usize * 4)
        .context("Could not allocate WebP RGBA buffer")?;
    let mut remaining = count;
    for band in bands {
        ensure!(!is_cancelled(), "Export cancelled");
        let band = band?;
        ensure!(
            band.len() as u64 <= remaining,
            "Too many pixels for WebP dimensions"
        );
        remaining -= band.len() as u64;
        for chunk in band.chunks(4096) {
            ensure!(!is_cancelled(), "Export cancelled");
            for &pixel in chunk {
                let index = rgba.len() as u64 / 4;
                rgba.extend_from_slice(&straight_srgb8_pixel(pixel, options.dither_seed, index));
            }
        }
    }
    ensure!(remaining == 0, "Not enough pixels for WebP dimensions");
    ensure!(!is_cancelled(), "Export cancelled");
    let mut encoder = image::codecs::webp::WebPEncoder::new_lossless(&mut output);
    encoder.set_icc_profile(ColorProfile::new_srgb().encode()?)?;
    if let Some(exif) = exif {
        encoder.set_exif_metadata(exif)?;
    }
    encoder.write_image(&rgba, width, height, image::ExtendedColorType::Rgba8)?;
    output.flush()?;
    Ok(())
}

fn straight_srgb8_pixel(pixel: [f16; 4], seed: Option<u64>, index: u64) -> [u8; 4] {
    let [r, g, b, a] = pixel.map(f16::to_f32);
    let rgb = rec2020_to_linear_srgb([r, g, b]);
    let mut output = [0; 4];
    for c in 0..3 {
        output[c] = quantize_srgb8_channel(
            linear_to_srgb(rgb[c]).clamp(0.0, 1.0) * 255.0,
            seed,
            index * 3 + c as u64,
        );
    }
    output[3] = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
    output
}

fn normalized_exif(exif: Option<&[u8]>, limit: usize) -> Result<Option<Vec<u8>>> {
    ensure!(
        exif.is_none_or(|bytes| bytes.len() <= limit),
        "EXIF metadata exceeds the export limit ({limit} bytes)"
    );
    exif.filter(|bytes| !bytes.is_empty())
        .map(|bytes| {
            exif::Reader::new()
                .read_raw(bytes.to_vec())
                .context("Invalid source EXIF metadata")?;
            let mut bytes = bytes.to_vec();
            let _ = image::metadata::Orientation::remove_from_exif_chunk(&mut bytes);
            Ok(bytes)
        })
        .transpose()
}

/// Encode oriented straight linear Rec.2020 bands as an 8-bit sRGB JPEG.
/// Retains validated EXIF, normalizes orientation and embeds an output sRGB ICC.
/// Uses an RGB8 CPU buffer (3 bytes/pixel, capped at 512 MiB), plus the current band.
/// JPEG's APP1 segment permits at most 65527 raw EXIF bytes; larger EXIF fails.
/// Caller owns cancellation of encoding writes and atomic file publication.
pub fn write_srgb8_jpeg(
    output: impl Write,
    width: u32,
    height: u32,
    exif: Option<&[u8]>,
    options: JpegOptions,
    bands: impl IntoIterator<Item = Result<Vec<[f16; 4]>>>,
) -> Result<()> {
    write_srgb8_jpeg_cancellable(output, width, height, exif, options, bands, || false)
}

/// Like [`write_srgb8_jpeg`], checking cancellation before allocation, between
/// bands, at most every 4096 converted pixels, and before encoding. The caller
/// must also check cancellation on output writes to interrupt the JPEG encoder.
pub fn write_srgb8_jpeg_cancellable(
    mut output: impl Write,
    width: u32,
    height: u32,
    exif: Option<&[u8]>,
    options: JpegOptions,
    bands: impl IntoIterator<Item = Result<Vec<[f16; 4]>>>,
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<()> {
    ensure!(!is_cancelled(), "Export cancelled");
    ensure!(
        width > 0 && height > 0 && width <= 65535 && height <= 65535,
        "JPEG dimensions must be between 1 and 65535"
    );
    ensure!(
        (1..=100).contains(&options.quality),
        "JPEG quality must be between 1 and 100"
    );
    let exif = normalized_exif(exif, 65527)?;
    let count = u64::from(width) * u64::from(height);
    ensure!(
        count * 3 <= 512 * 1024 * 1024,
        "JPEG RGB buffer exceeds the 512 MiB limit"
    );
    let mut rgb = Vec::new();
    rgb.try_reserve_exact(count as usize * 3)
        .context("Could not allocate JPEG RGB buffer")?;
    let background = options
        .background
        .map(|c| srgb_to_linear(f32::from(c) / 255.0));
    let mut remaining = count;
    for band in bands {
        ensure!(!is_cancelled(), "Export cancelled");
        let band = band?;
        ensure!(
            band.len() as u64 <= remaining,
            "Too many pixels for JPEG dimensions"
        );
        remaining -= band.len() as u64;
        for chunk in band.chunks(4096) {
            ensure!(!is_cancelled(), "Export cancelled");
            for &pixel in chunk {
                let index = rgb.len() as u64 / 3;
                rgb.extend_from_slice(&jpeg_pixel(pixel, background, options.dither_seed, index));
            }
        }
    }
    ensure!(remaining == 0, "Not enough pixels for JPEG dimensions");
    ensure!(!is_cancelled(), "Export cancelled");
    let mut encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, options.quality);
    encoder.set_icc_profile(ColorProfile::new_srgb().encode()?)?;
    if let Some(exif) = exif {
        encoder.set_exif_metadata(exif)?;
    }
    encoder.write_image(&rgb, width, height, image::ExtendedColorType::Rgb8)?;
    output.flush()?;
    Ok(())
}

fn jpeg_pixel(
    pixel: [f16; 4],
    background: [f32; 3],
    dither_seed: Option<u64>,
    index: u64,
) -> [u8; 3] {
    let [r, g, b, a] = pixel.map(f16::to_f32);
    let alpha = if a.is_finite() {
        a.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let rgb = rec2020_to_linear_srgb([r, g, b]);
    std::array::from_fn(|c| {
        let background = background[c];
        // Ignore hidden color entirely at zero alpha, including non-finite RGB.
        let linear = if alpha == 0.0 {
            background
        } else {
            rgb[c] * alpha + background * (1.0 - alpha)
        };
        let value = linear_to_srgb(linear).clamp(0.0, 1.0) * 255.0;
        quantize_srgb8_channel(value, dither_seed, index * 3 + c as u64)
    })
}

fn quantize_srgb8_channel(value: f32, dither_seed: Option<u64>, sample: u64) -> u8 {
    let nearest = value.round();
    // Preserve representable levels, including black/white and clipped values.
    // Allow small transfer/matrix rounding error (less than 0.0001 of a code step).
    if (value - nearest).abs() <= 0.0001 {
        return nearest as u8;
    }
    let noise = dither_seed.map_or(0.0, |seed| triangular_noise(seed, sample));
    (value + noise).round().clamp(0.0, 255.0) as u8
}

fn triangular_noise(seed: u64, sample: u64) -> f32 {
    // SplitMix64 finalizer: two independent uniforms yield triangular [-1, 1] noise.
    let uniform = |offset| {
        let mut v = seed.wrapping_add(
            sample
                .wrapping_mul(2)
                .wrapping_add(offset)
                .wrapping_mul(0x9e3779b97f4a7c15),
        );
        v = (v ^ (v >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        v = (v ^ (v >> 27)).wrapping_mul(0x94d049bb133111eb);
        ((v ^ (v >> 31)) >> 40) as f32 / 16777216.0
    };
    uniform(0) - uniform(1)
}

/// Stream row-major straight linear Rec.2020 pixels to a 16-bit RGBA sRGB PNG.
/// Quantization clips to the initial sRGB output gamut, matching the viewport.
/// Embeds the output ICC profile without EXIF. Caller owns atomic file publication.
pub fn write_srgb16_png(
    output: impl Write,
    width: u32,
    height: u32,
    bands: impl IntoIterator<Item = Result<Vec<[f16; 4]>>>,
) -> Result<()> {
    write_srgb16_png_with_exif(output, width, height, None, bands)
}

/// Like [`write_srgb16_png`], preserving source EXIF from [`image::ImageDecoder`].
/// Pixels must already be oriented. Recognized IFD0 orientation is reset to 1 in
/// an export-owned copy; all other metadata bytes are retained, including unknown
/// tags. Missing or unrecognized orientation is left unchanged, as on decode.
/// Empty EXIF is omitted; malformed nonempty EXIF fails before any output is written.
/// This does not rewrite other source descriptions (dimensions, thumbnails, etc.).
pub fn write_srgb16_png_with_exif(
    output: impl Write,
    width: u32,
    height: u32,
    exif: Option<&[u8]>,
    bands: impl IntoIterator<Item = Result<Vec<[f16; 4]>>>,
) -> Result<()> {
    write_srgb_png(output, width, height, exif, PngOptions::default(), bands)
}

/// Stream already oriented straight linear Rec.2020 bands to RGBA sRGB PNG.
/// Embeds sRGB ICC and validated, orientation-normalized EXIF. Eight-bit RGB
/// optionally uses seeded triangular dithering; alpha is rounded without noise.
/// Sixteen-bit output never dithers. Caller owns atomic file publication.
pub fn write_srgb_png(
    output: impl Write,
    width: u32,
    height: u32,
    exif: Option<&[u8]>,
    options: PngOptions,
    bands: impl IntoIterator<Item = Result<Vec<[f16; 4]>>>,
) -> Result<()> {
    write_srgb_png_cancellable(output, width, height, exif, options, bands, || false)
}

/// Like [`write_srgb_png`], checking cancellation before writing the header,
/// between bands and at most every 4096 converted pixels. The caller must also
/// check output writes to interrupt compression and finalization.
pub fn write_srgb_png_cancellable(
    output: impl Write,
    width: u32,
    height: u32,
    exif: Option<&[u8]>,
    options: PngOptions,
    bands: impl IntoIterator<Item = Result<Vec<[f16; 4]>>>,
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<()> {
    ensure!(!is_cancelled(), "Export cancelled");
    ensure!(width > 0 && height > 0, "Invalid PNG dimensions");
    let exif = normalized_exif(exif, crate::MAX_METADATA_BYTES)?;
    let mut info = png::Info::with_size(width, height);
    info.color_type = png::ColorType::Rgba;
    info.bit_depth = match options.bit_depth {
        PngBitDepth::Eight => png::BitDepth::Eight,
        PngBitDepth::Sixteen => png::BitDepth::Sixteen,
    };
    info.icc_profile = Some(Cow::Owned(ColorProfile::new_srgb().encode()?));
    info.exif_metadata = exif.map(Cow::Owned);
    let mut writer = png::Encoder::with_info(output, info)?.write_header()?;
    let mut remaining = u64::from(width) * u64::from(height);
    let mut index = 0_u64;
    {
        let mut stream = writer.stream_writer()?;
        let mut bytes = Vec::with_capacity(4096 * 8);
        for band in bands {
            ensure!(!is_cancelled(), "Export cancelled");
            let band = band?;
            ensure!(
                band.len() as u64 <= remaining,
                "Too many pixels for PNG dimensions"
            );
            remaining -= band.len() as u64;
            for chunk in band.chunks(4096) {
                ensure!(!is_cancelled(), "Export cancelled");
                bytes.clear();
                for pixel in chunk {
                    match options.bit_depth {
                        PngBitDepth::Eight => {
                            bytes.extend_from_slice(&straight_srgb8_pixel(
                                *pixel,
                                options.dither_seed,
                                index,
                            ));
                        }
                        PngBitDepth::Sixteen => {
                            let [r, g, b, a] = pixel.map(f16::to_f32);
                            let rgb = rec2020_to_linear_srgb([r, g, b])
                                .map(|c| linear_to_srgb(c).clamp(0.0, 1.0));
                            let alpha = a.clamp(0.0, 1.0);
                            for channel in rgb.into_iter().chain([alpha]) {
                                bytes.extend_from_slice(
                                    &((channel * 65535.0).round() as u16).to_be_bytes(),
                                );
                            }
                        }
                    }
                    index += 1;
                }
                stream.write_all(&bytes)?;
            }
        }
        ensure!(remaining == 0, "Not enough pixels for PNG dimensions");
        ensure!(!is_cancelled(), "Export cancelled");
        stream.finish()?;
    }
    // Explicit finish propagates final IEND and flush errors (Drop cannot do that).
    writer.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageDecoder;
    use std::io::Cursor;

    // IFD0 contains an out-of-line camera Make and optionally Orientation.
    // Keeping the Make before Orientation also exercises scanning unrelated tags.
    fn source_exif(little_endian: bool, orientation: Option<u16>) -> Vec<u8> {
        let short = |v: u16| {
            if little_endian {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        };
        let long = |v: u32| {
            if little_endian {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        };
        let entries = if orientation.is_some() { 2 } else { 1 };
        let mut bytes = Vec::new();
        bytes.extend_from_slice(if little_endian { b"II" } else { b"MM" });
        bytes.extend_from_slice(&short(42));
        bytes.extend_from_slice(&long(8));
        bytes.extend_from_slice(&short(entries));
        bytes.extend_from_slice(&short(0x10f)); // Make, ASCII, six bytes including NUL.
        bytes.extend_from_slice(&short(2));
        bytes.extend_from_slice(&long(6));
        bytes.extend_from_slice(&long(8 + 2 + u32::from(entries) * 12 + 4));
        if let Some(orientation) = orientation {
            bytes.extend_from_slice(&short(0x112));
            bytes.extend_from_slice(&short(3));
            bytes.extend_from_slice(&long(1));
            bytes.extend_from_slice(&short(orientation));
            bytes.extend_from_slice(&[0, 0]);
        }
        bytes.extend_from_slice(&long(0));
        bytes.extend_from_slice(b"Canon\0");
        bytes
    }

    fn png_bytes(
        options: PngOptions,
        bands: Vec<Result<Vec<[f16; 4]>>>,
        width: u32,
        height: u32,
    ) -> Vec<u8> {
        let mut output = Vec::new();
        write_srgb_png(&mut output, width, height, None, options, bands).unwrap();
        output
    }

    #[test]
    fn webp_preserves_straight_rgba_and_normalized_metadata() {
        let pixels: Vec<_> = [0.0, 0.25, 0.5, 1.0]
            .into_iter()
            .map(|a| [0.25, 0.25, 0.25, a].map(f16::from_f32))
            .collect();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("straight.webp");
        for little_endian in [true, false] {
            for orientation in 1..=8 {
                let source = source_exif(little_endian, Some(orientation));
                write_srgb8_webp(
                    std::fs::File::create(&path).unwrap(),
                    2,
                    2,
                    Some(&source),
                    WebPOptions { dither_seed: None },
                    [Ok(pixels.clone())],
                )
                .unwrap();
                let mut decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(
                    std::fs::read(&path).unwrap(),
                ))
                .unwrap();
                assert_eq!(decoder.dimensions(), (2, 2));
                assert_eq!(decoder.color_type(), image::ColorType::Rgba8);
                let profile =
                    ColorProfile::new_from_slice(&decoder.icc_profile().unwrap().unwrap()).unwrap();
                let mut expected = ColorProfile::new_srgb();
                expected.creation_date_time = profile.creation_date_time;
                assert_eq!(profile.encode().unwrap(), expected.encode().unwrap());
                let mut normalized = source.clone();
                let _ = image::metadata::Orientation::remove_from_exif_chunk(&mut normalized);
                assert_eq!(decoder.exif_metadata().unwrap().unwrap(), normalized);
                let rgba = image::DynamicImage::from_decoder(decoder)
                    .unwrap()
                    .into_rgba8();
                for (actual, alpha) in rgba.pixels().zip([0, 64, 128, 255]) {
                    assert_eq!(actual.0, [137, 137, 137, alpha]);
                }
                let reloaded = crate::decode(&path).unwrap();
                assert_eq!(
                    reloaded.source_orientation,
                    image::metadata::Orientation::NoTransforms
                );
                assert_eq!((reloaded.buffer.width(), reloaded.buffer.height()), (2, 2));
                assert_eq!(source, source_exif(little_endian, Some(orientation)));
            }
        }
    }

    #[test]
    fn webp_is_lossless_after_quantization_independent_of_band_boundaries() {
        let pixels: Vec<_> = (0..131 * 67)
            .map(|i| {
                let value = (i % 257) as f32 / 256.0;
                [value, 0.18, 1.0 - value, [0.0, 0.25, 0.5, 1.0][i % 4]].map(f16::from_f32)
            })
            .collect();
        let mut previous = None;
        for seed in [None, Some(0), Some(42)] {
            let expected = image::load_from_memory(&png_bytes(
                PngOptions {
                    bit_depth: PngBitDepth::Eight,
                    dither_seed: seed,
                },
                vec![Ok(pixels.clone())],
                131,
                67,
            ))
            .unwrap()
            .into_rgba8();
            for band_size in [71, pixels.len()] {
                let mut output = Vec::new();
                write_srgb8_webp(
                    &mut output,
                    131,
                    67,
                    None,
                    WebPOptions { dither_seed: seed },
                    pixels.chunks(band_size).map(|p| Ok(p.to_vec())),
                )
                .unwrap();
                let actual = image::load_from_memory(&output).unwrap().into_rgba8();
                assert_eq!(
                    actual, expected,
                    "WebP must retain every quantized RGBA byte"
                );
                for (i, p) in actual.pixels().enumerate() {
                    assert_eq!(p[3], [0, 64, 128, 255][i % 4]);
                }
            }
            if let Some(previous) = previous {
                assert_ne!(expected, previous);
            }
            previous = Some(expected);
        }
    }

    #[test]
    fn webp_validates_limits_metadata_and_pixel_stream_before_writing() {
        let oversized = vec![0; crate::MAX_METADATA_BYTES + 1];
        for (w, h, exif) in [
            (0, 1, None),
            (1, 0, None),
            (16385, 1, None),
            (1, 16385, None),
            (16384, 8193, None), // Four-byte output exceeds 512 MiB.
            (1, 1, Some(&[0; 7][..])),
            (1, 1, Some(oversized.as_slice())),
        ] {
            let bands = std::iter::from_fn(|| -> Option<Result<Vec<[f16; 4]>>> {
                panic!("invalid inputs must fail before requesting pixels")
            });
            let mut output = Vec::new();
            assert!(
                write_srgb8_webp(&mut output, w, h, exif, WebPOptions::default(), bands).is_err()
            );
            assert!(output.is_empty());
        }
        for bands in [
            vec![Ok(vec![])],
            vec![Ok(vec![[f16::ONE; 4]; 2])],
            vec![Err(anyhow::anyhow!("producer failed"))],
        ] {
            let mut output = Vec::new();
            assert!(
                write_srgb8_webp(&mut output, 1, 1, None, WebPOptions::default(), bands).is_err()
            );
            assert!(output.is_empty());
        }
        for (w, h) in [(16384, 1), (1, 16384)] {
            let mut output = Vec::new();
            write_srgb8_webp(
                &mut output,
                w,
                h,
                Some(&[]),
                WebPOptions::default(),
                [Ok(vec![[f16::ONE; 4]; (w * h) as usize])],
            )
            .unwrap();
            let mut decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(output)).unwrap();
            assert_eq!(decoder.dimensions(), (w, h));
            assert!(decoder.exif_metadata().unwrap().is_none());
            let rgba = image::DynamicImage::from_decoder(decoder)
                .unwrap()
                .into_rgba8();
            assert!(rgba.pixels().all(|p| p.0 == [255; 4]));
        }
    }

    #[test]
    fn webp_cancellation_interrupts_conversion_and_prevents_encoding() {
        for cancel_at in [1, 4, 7] {
            let mut checkpoints = 0;
            let mut output = Vec::new();
            let bands = std::iter::once_with(|| {
                assert!(cancel_at > 1, "initial cancellation must not consume bands");
                Ok(vec![[f16::ONE; 4]; 256 * 64])
            });
            let error = write_srgb8_webp_cancellable(
                &mut output,
                256,
                64,
                None,
                WebPOptions::default(),
                bands,
                || {
                    checkpoints += 1;
                    checkpoints == cancel_at
                },
            )
            .unwrap_err();
            assert_eq!(error.to_string(), "Export cancelled");
            assert_eq!(checkpoints, cancel_at);
            assert!(output.is_empty());
        }
    }

    #[test]
    fn webp_propagates_output_and_flush_failures() {
        struct FailingWriter {
            fail_on_flush: bool,
        }
        impl Write for FailingWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.fail_on_flush {
                    Ok(bytes.len())
                } else {
                    Err(std::io::Error::other("write failed"))
                }
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::other("flush failed"))
            }
        }
        for fail_on_flush in [false, true] {
            let error = write_srgb8_webp(
                FailingWriter { fail_on_flush },
                1,
                1,
                None,
                WebPOptions::default(),
                [Ok(vec![[f16::ONE; 4]])],
            )
            .unwrap_err();
            assert!(error.to_string().contains(if fail_on_flush {
                "flush failed"
            } else {
                "write failed"
            }));
        }
    }

    #[test]
    fn png8_preserves_straight_color_alpha_and_normalized_metadata() {
        let options = PngOptions {
            bit_depth: PngBitDepth::Eight,
            dither_seed: None,
        };
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rgba8.png");
        let pixels: Vec<_> = [0.0, 0.25, 0.5, 1.0]
            .into_iter()
            .map(|alpha| [0.25, 0.25, 0.25, alpha].map(f16::from_f32))
            .collect();
        for little_endian in [true, false] {
            for orientation in 1..=8 {
                let exif = source_exif(little_endian, Some(orientation));
                write_srgb_png(
                    std::fs::File::create(&path).unwrap(),
                    2,
                    2,
                    Some(&exif),
                    options,
                    [Ok(pixels.clone())],
                )
                .unwrap();
                let mut decoder =
                    image::codecs::png::PngDecoder::new(Cursor::new(std::fs::read(&path).unwrap()))
                        .unwrap();
                assert_eq!(decoder.color_type(), image::ColorType::Rgba8);
                assert_eq!(decoder.dimensions(), (2, 2));
                let profile =
                    ColorProfile::new_from_slice(&decoder.icc_profile().unwrap().unwrap()).unwrap();
                let mut expected_profile = ColorProfile::new_srgb();
                expected_profile.creation_date_time = profile.creation_date_time;
                assert_eq!(
                    profile.encode().unwrap(),
                    expected_profile.encode().unwrap()
                );
                let mut normalized = exif.clone();
                let _ = image::metadata::Orientation::remove_from_exif_chunk(&mut normalized);
                assert_eq!(decoder.exif_metadata().unwrap().unwrap(), normalized);
                let rgba = image::DynamicImage::from_decoder(decoder)
                    .unwrap()
                    .into_rgba8();
                for (actual, alpha) in rgba.pixels().zip([0, 64, 128, 255]) {
                    assert_eq!(actual.0, [137, 137, 137, alpha]);
                }
                let reloaded = crate::decode(&path).unwrap();
                assert_eq!(
                    reloaded.source_orientation,
                    image::metadata::Orientation::NoTransforms
                );
                assert_eq!((reloaded.buffer.width(), reloaded.buffer.height()), (2, 2));
                assert_eq!(exif, source_exif(little_endian, Some(orientation)));
            }
        }
    }

    #[test]
    fn png8_dither_is_seeded_band_independent_and_never_changes_alpha() {
        let pixels: Vec<_> = (0..131 * 67)
            .map(|i| [0.18, 0.18, 0.18, [0.0, 0.25, 0.5, 1.0][i % 4]].map(f16::from_f32))
            .collect();
        let options = PngOptions {
            bit_depth: PngBitDepth::Eight,
            dither_seed: Some(42),
        };
        let decode = |bytes: Vec<u8>| image::load_from_memory(&bytes).unwrap().into_rgba8();
        let first = decode(png_bytes(options, vec![Ok(pixels.clone())], 131, 67));
        let split = decode(png_bytes(
            options,
            pixels.chunks(71).map(|p| Ok(p.to_vec())).collect(),
            131,
            67,
        ));
        assert_eq!(first, split);
        let second = decode(png_bytes(
            PngOptions {
                dither_seed: Some(43),
                ..options
            },
            vec![Ok(pixels.clone())],
            131,
            67,
        ));
        assert_ne!(first, second);
        let plain = decode(png_bytes(
            PngOptions {
                dither_seed: None,
                ..options
            },
            vec![Ok(pixels.clone())],
            131,
            67,
        ));
        let expected = linear_to_srgb(pixels[0][0].to_f32()) * 255.0;
        let mut sum = 0.0;
        for (i, ((a, b), p)) in first
            .pixels()
            .zip(second.pixels())
            .zip(plain.pixels())
            .enumerate()
        {
            assert_eq!(a[3], [0, 64, 128, 255][i % 4]);
            assert_eq!(a[3], b[3]);
            assert_eq!(a[3], p[3]);
            assert_eq!(&p.0[..3], &[expected.round() as u8; 3]);
            for c in 0..3 {
                assert!((f32::from(a[c]) - expected).abs() <= 1.5);
                sum += f32::from(a[c]) - expected;
            }
        }
        assert!(sum.abs() / ((pixels.len() * 3) as f32) < 0.02);
        assert!(
            first
                .pixels()
                .any(|p| p.0[..3] != plain.get_pixel(0, 0).0[..3])
        );
        // At 16 bits all dither settings must produce identical decoded pixels.
        let decode16 = |seed| {
            image::load_from_memory(&png_bytes(
                PngOptions {
                    bit_depth: PngBitDepth::Sixteen,
                    dither_seed: seed,
                },
                vec![Ok(pixels.clone())],
                131,
                67,
            ))
            .unwrap()
            .into_rgba16()
        };
        assert_eq!(decode16(Some(42)), decode16(None));
    }

    #[test]
    fn png8_and_webp_clip_endpoints_and_handle_nonfinite_samples_without_noise() {
        let pixels: Vec<_> = [
            ([-4.0, -4.0, -4.0, -1.0], [0, 0, 0, 0]),
            ([0.0, 0.0, 0.0, 0.0], [0, 0, 0, 0]),
            ([1.0, 1.0, 1.0, 1.0], [255; 4]),
            ([4.0, 4.0, 4.0, 2.0], [255; 4]),
            ([f32::NAN; 4], [0; 4]),
        ]
        .into_iter()
        .collect();
        let bytes = png_bytes(
            PngOptions {
                bit_depth: PngBitDepth::Eight,
                ..Default::default()
            },
            vec![Ok(pixels
                .iter()
                .map(|(p, _)| p.map(f16::from_f32))
                .collect())],
            pixels.len() as u32,
            1,
        );
        let mut webp = Vec::new();
        write_srgb8_webp(
            &mut webp,
            pixels.len() as u32,
            1,
            None,
            WebPOptions::default(),
            [Ok(pixels
                .iter()
                .map(|(p, _)| p.map(f16::from_f32))
                .collect())],
        )
        .unwrap();
        for encoded in [bytes, webp] {
            let rgba = image::load_from_memory(&encoded).unwrap().into_rgba8();
            for (actual, (_, expected)) in rgba.pixels().zip(&pixels) {
                assert_eq!(actual.0, *expected);
            }
        }
    }

    #[test]
    fn png_options_reject_bad_metadata_dimensions_and_streams() {
        let oversized = vec![0; crate::MAX_METADATA_BYTES + 1];
        for bit_depth in [PngBitDepth::Eight, PngBitDepth::Sixteen] {
            let options = PngOptions {
                bit_depth,
                ..Default::default()
            };
            for (w, h, exif) in [
                (0, 1, None),
                (1, 0, None),
                (1, 1, Some(&[0; 7][..])),
                (1, 1, Some(oversized.as_slice())),
            ] {
                let mut output = Vec::new();
                assert!(
                    write_srgb_png(&mut output, w, h, exif, options, [Ok(vec![[f16::ONE; 4]])])
                        .is_err()
                );
                assert!(output.is_empty());
            }
            for bands in [
                vec![Ok(vec![])],
                vec![Ok(vec![[f16::ONE; 4]; 2])],
                vec![Err(anyhow::anyhow!("producer failed"))],
            ] {
                assert!(write_srgb_png(Vec::new(), 1, 1, None, options, bands).is_err());
            }
            let mut output = Vec::new();
            write_srgb_png(
                &mut output,
                1,
                1,
                Some(&[]),
                options,
                [Ok(vec![[f16::ONE; 4]])],
            )
            .unwrap();
            let mut decoder = image::codecs::png::PngDecoder::new(Cursor::new(output)).unwrap();
            assert!(decoder.exif_metadata().unwrap().is_none());
        }
    }

    #[test]
    fn png_cancellation_interrupts_conversion_within_a_band() {
        for bit_depth in [PngBitDepth::Eight, PngBitDepth::Sixteen] {
            let options = PngOptions {
                bit_depth,
                ..Default::default()
            };
            let mut checkpoints = 0;
            let mut output = Vec::new();
            let error = write_srgb_png_cancellable(
                &mut output,
                256,
                64,
                None,
                options,
                [Ok(vec![[f16::ONE; 4]; 256 * 64])],
                || {
                    checkpoints += 1;
                    checkpoints == 4
                },
            )
            .unwrap_err();
            assert_eq!(error.to_string(), "Export cancelled");
            assert_eq!(checkpoints, 4);
            assert!(
                image::load_from_memory(&output).is_err(),
                "must not complete the PNG"
            );
            let mut output = Vec::new();
            let bands = std::iter::from_fn(|| -> Option<Result<Vec<[f16; 4]>>> {
                panic!("must not request pixels")
            });
            assert!(
                write_srgb_png_cancellable(&mut output, 1, 1, None, options, bands, || true)
                    .is_err()
            );
            assert!(output.is_empty());
        }
    }

    #[test]
    fn jpeg_cancellation_interrupts_conversion_within_one_large_band() {
        let mut output = Vec::new();
        let mut requests = 0;
        let bands = std::iter::from_fn(|| {
            requests += 1;
            assert_eq!(requests, 1, "cancel before requesting the next band");
            Some(Ok(vec![[f16::ONE; 4]; 256 * 64]))
        });
        let mut checkpoints = 0;
        let error = write_srgb8_jpeg_cancellable(
            &mut output,
            256,
            64,
            None,
            JpegOptions::default(),
            bands,
            || {
                checkpoints += 1;
                // Initial check, band check, first conversion chunk, second chunk.
                checkpoints == 4
            },
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "Export cancelled");
        assert_eq!(checkpoints, 4);
        assert_eq!(requests, 1);
        assert!(
            output.is_empty(),
            "cancel conversion before encoding starts"
        );

        let bands = std::iter::from_fn(|| -> Option<Result<Vec<[f16; 4]>>> {
            panic!("an already cancelled export must not request pixels")
        });
        assert!(
            write_srgb8_jpeg_cancellable(
                &mut output,
                1,
                1,
                None,
                JpegOptions::default(),
                bands,
                || true,
            )
            .is_err()
        );
        assert!(output.is_empty());
    }

    #[test]
    fn jpeg_dither_preserves_solid_backgrounds_and_clipped_black_and_white() {
        for seed in [0, 42, u64::MAX] {
            for index in 0..256 {
                let background = [index as u8, (255 - index) as u8, (index / 2) as u8];
                let linear = background.map(|c| srgb_to_linear(f32::from(c) / 255.0));
                assert_eq!(
                    jpeg_pixel(
                        [f16::NAN, f16::INFINITY, f16::NEG_INFINITY, f16::ZERO],
                        linear,
                        Some(seed),
                        index
                    ),
                    background,
                );
                for (value, expected) in [(-4.0, 0), (0.0, 0), (1.0, 255), (4.0, 255)] {
                    assert_eq!(
                        jpeg_pixel(
                            [value, value, value, 1.0].map(f16::from_f32),
                            linear,
                            Some(seed),
                            index
                        ),
                        [expected; 3]
                    );
                }
            }
        }
        // Verify default dithering does not create AC detail in uniform JPEGs.
        for (pixel, expected) in [
            ([0.0, 0.0, 0.0, 0.0], 255),
            ([1.0; 4], 255),
            ([0.0, 0.0, 0.0, 1.0], 0),
        ] {
            let mut output = Vec::new();
            write_srgb8_jpeg(
                &mut output,
                64,
                64,
                None,
                JpegOptions::default(),
                [Ok(vec![pixel.map(f16::from_f32); 64 * 64])],
            )
            .unwrap();
            let rgb = image::load_from_memory(&output).unwrap().into_rgb8();
            assert!(rgb.pixels().all(|p| p.0 == [expected; 3]));
        }
    }

    #[test]
    fn jpeg_nonfinite_rgb_quantizes_without_panicking() {
        // A NaN input to f32::clamp propagates NaN; only invalid bounds panic.
        // JPEG quantization maps NaN color to zero through Rust's saturating cast.
        for alpha in [0.5, 1.0] {
            let pixel = [f16::NAN, f16::NAN, f16::NAN, f16::from_f32(alpha)];
            assert_eq!(jpeg_pixel(pixel, [1.0; 3], Some(0), 0), [0; 3]);
            let mut output = Vec::new();
            write_srgb8_jpeg(
                &mut output,
                8,
                8,
                None,
                JpegOptions::default(),
                [Ok(vec![pixel; 64])],
            )
            .unwrap();
            let rgb = image::load_from_memory(&output).unwrap().into_rgb8();
            assert!(rgb.pixels().all(|p| p.0 == [0; 3]));
        }
    }

    #[test]
    fn jpeg_composites_linear_light_and_preserves_profile_and_normalized_exif() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("export.jpg");
        let options = JpegOptions {
            quality: 100,
            background: [128, 64, 192],
            dither_seed: None,
        };
        for little_endian in [true, false] {
            for orientation in 1..=8 {
                let exif = source_exif(little_endian, Some(orientation));
                // Uniform patches avoid JPEG ringing; transparency must ignore hidden RGB.
                for (pixel, expected) in [
                    ([0.0, 0.0, 0.0, 0.5], [92, 44, 140]),
                    ([8.0, -2.0, 4.0, 0.0], options.background),
                    ([0.25, 0.25, 0.25, 1.0], [137; 3]),
                ] {
                    write_srgb8_jpeg(
                        std::fs::File::create(&path).unwrap(),
                        17,
                        9,
                        Some(&exif),
                        options,
                        [Ok(vec![pixel.map(f16::from_f32); 17 * 9])],
                    )
                    .unwrap();
                    let mut decoder = image::codecs::jpeg::JpegDecoder::new(Cursor::new(
                        std::fs::read(&path).unwrap(),
                    ))
                    .unwrap();
                    assert_eq!(decoder.dimensions(), (17, 9));
                    assert_eq!(decoder.color_type(), image::ColorType::Rgb8);
                    let profile =
                        ColorProfile::new_from_slice(&decoder.icc_profile().unwrap().unwrap())
                            .unwrap();
                    let mut expected_profile = ColorProfile::new_srgb();
                    expected_profile.creation_date_time = profile.creation_date_time;
                    assert_eq!(
                        profile.encode().unwrap(),
                        expected_profile.encode().unwrap()
                    );
                    let mut normalized = exif.clone();
                    let _ = image::metadata::Orientation::remove_from_exif_chunk(&mut normalized);
                    assert_eq!(decoder.exif_metadata().unwrap().unwrap(), normalized);
                    let rgb = image::DynamicImage::from_decoder(decoder)
                        .unwrap()
                        .into_rgb8();
                    for actual in rgb.pixels() {
                        for c in 0..3 {
                            assert!(
                                (i16::from(actual[c]) - i16::from(expected[c])).abs() <= 2,
                                "{actual:?} vs {expected:?}"
                            );
                        }
                    }
                    let reloaded = crate::decode(&path).unwrap();
                    assert_eq!(
                        reloaded.source_orientation,
                        image::metadata::Orientation::NoTransforms
                    );
                    assert_eq!((reloaded.buffer.width(), reloaded.buffer.height()), (17, 9));
                    assert!(reloaded.buffer.pixels().iter().all(|p| p[3] == f16::ONE));
                    assert_eq!(exif, source_exif(little_endian, Some(orientation)));
                }
            }
        }
    }

    #[test]
    fn jpeg_quality_changes_compression_and_dither_is_seeded_and_band_independent() {
        let pixels: Vec<_> = (0..64 * 33)
            .map(|i| {
                let value = ((i * 37 % 256) as f32 / 255.0) * 0.5;
                [value, value, value, 1.0].map(f16::from_f32)
            })
            .collect();
        let encode = |quality, bands| {
            let mut output = Vec::new();
            write_srgb8_jpeg(
                &mut output,
                64,
                33,
                None,
                JpegOptions {
                    quality,
                    ..Default::default()
                },
                bands,
            )
            .unwrap();
            output
        };
        let high = encode(100, vec![Ok(pixels.clone())]);
        let low = encode(10, vec![Ok(pixels.clone())]);
        assert!(high.len() > low.len() * 2);
        let split = encode(100, pixels.chunks(71).map(|p| Ok(p.to_vec())).collect());
        let decode = |bytes: Vec<u8>| image::load_from_memory(&bytes).unwrap().into_rgb8();
        assert_eq!(decode(high), decode(split));
        let noise: Vec<_> = (0..10000).map(|i| triangular_noise(42, i)).collect();
        assert!(noise.iter().all(|v| (-1.0..=1.0).contains(v)));
        assert!(noise.iter().sum::<f32>().abs() / 10000.0 < 0.02);
        let pixel = [0.18, 0.18, 0.18, 1.0].map(f16::from_f32);
        let first: Vec<_> = (0..100)
            .map(|i| jpeg_pixel(pixel, [1.0; 3], Some(42), i))
            .collect();
        let second: Vec<_> = (0..100)
            .map(|i| jpeg_pixel(pixel, [1.0; 3], Some(43), i))
            .collect();
        assert_ne!(first, second);
        assert!(first.windows(2).any(|p| p[0] != p[1]));
        assert_eq!(jpeg_pixel([f16::NAN; 4], [1.0; 3], None, 0), [255; 3]);
    }

    #[test]
    fn jpeg_rejects_invalid_options_metadata_and_bands_before_output() {
        let valid = source_exif(true, Some(6));
        let mut at_limit = valid.clone();
        at_limit.resize(65527, 0);
        let mut output = Vec::new();
        write_srgb8_jpeg(
            &mut output,
            1,
            1,
            Some(&at_limit),
            JpegOptions::default(),
            [Ok(vec![[f16::ONE; 4]])],
        )
        .unwrap();
        let mut decoder = image::codecs::jpeg::JpegDecoder::new(Cursor::new(output)).unwrap();
        assert_eq!(decoder.exif_metadata().unwrap().unwrap().len(), 65527);
        let mut oversized = at_limit;
        oversized.push(0);
        for (width, height, exif, quality) in [
            (0, 1, None, 90),
            (65536, 1, None, 90),
            (65535, 65535, None, 90),
            (1, 1, None, 0),
            (1, 1, None, 101),
            (1, 1, Some(&[0; 7][..]), 90),
            (1, 1, Some(oversized.as_slice()), 90),
        ] {
            let mut output = Vec::new();
            assert!(
                write_srgb8_jpeg(
                    &mut output,
                    width,
                    height,
                    exif,
                    JpegOptions {
                        quality,
                        ..Default::default()
                    },
                    [Ok(vec![[f16::ONE; 4]])]
                )
                .is_err()
            );
            assert!(output.is_empty());
        }
        for bands in [
            vec![Ok(vec![])],
            vec![Ok(vec![[f16::ONE; 4]; 2])],
            vec![Err(anyhow::anyhow!("producer failed"))],
        ] {
            let mut output = Vec::new();
            assert!(
                write_srgb8_jpeg(&mut output, 1, 1, None, JpegOptions::default(), bands).is_err()
            );
            assert!(output.is_empty());
        }
        let mut output = Vec::new();
        write_srgb8_jpeg(
            &mut output,
            1,
            1,
            Some(&[]),
            JpegOptions::default(),
            [Ok(vec![[f16::ONE; 4]])],
        )
        .unwrap();
        let mut decoder = image::codecs::jpeg::JpegDecoder::new(Cursor::new(output)).unwrap();
        assert!(decoder.exif_metadata().unwrap().is_none());
    }

    #[test]
    fn oriented_sources_export_and_reload_once_with_camera_metadata() {
        use image::{ImageEncoder, metadata::Orientation};
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("source");
        let output = directory.path().join("export.png");
        let pixels: Vec<u8> = [30, 65, 100, 135, 170, 205]
            .into_iter()
            .flat_map(|gray| [gray; 3])
            .collect();
        for little_endian in [true, false] {
            for orientation in 1..=8 {
                for format in [
                    image::ImageFormat::Png,
                    image::ImageFormat::Jpeg,
                    image::ImageFormat::WebP,
                ] {
                    let exif = source_exif(little_endian, Some(orientation));
                    let file = std::fs::File::create(&input).unwrap();
                    match format {
                        image::ImageFormat::Png => {
                            let mut encoder = image::codecs::png::PngEncoder::new(file);
                            encoder.set_exif_metadata(exif.clone()).unwrap();
                            encoder
                                .write_image(&pixels, 2, 3, image::ExtendedColorType::Rgb8)
                                .unwrap();
                        }
                        image::ImageFormat::Jpeg => {
                            let mut encoder =
                                image::codecs::jpeg::JpegEncoder::new_with_quality(file, 100);
                            encoder.set_exif_metadata(exif.clone()).unwrap();
                            encoder
                                .write_image(&pixels, 2, 3, image::ExtendedColorType::Rgb8)
                                .unwrap();
                        }
                        image::ImageFormat::WebP => {
                            let mut encoder = image::codecs::webp::WebPEncoder::new_lossless(file);
                            encoder.set_exif_metadata(exif.clone()).unwrap();
                            encoder
                                .write_image(&pixels, 2, 3, image::ExtendedColorType::Rgb8)
                                .unwrap();
                        }
                        _ => unreachable!(),
                    }
                    let decoded = crate::decode(&input).unwrap();
                    assert_eq!(
                        decoded.source_orientation,
                        Orientation::from_exif(orientation as u8).unwrap()
                    );
                    write_srgb16_png_with_exif(
                        std::fs::File::create(&output).unwrap(),
                        decoded.buffer.width(),
                        decoded.buffer.height(),
                        decoded.exif_bytes.as_deref(),
                        [Ok(decoded.buffer.pixels().to_vec())],
                    )
                    .unwrap();
                    let reloaded = crate::decode(&output).unwrap();
                    assert_eq!(reloaded.source_orientation, Orientation::NoTransforms);
                    assert_eq!(reloaded.buffer.width(), decoded.buffer.width());
                    assert_eq!(reloaded.buffer.height(), decoded.buffer.height());
                    let mut expected = exif.clone();
                    let _ = Orientation::remove_from_exif_chunk(&mut expected);
                    assert_eq!(reloaded.exif_bytes.as_deref(), Some(expected.as_slice()));
                    assert_eq!(decoded.exif_bytes.as_deref(), Some(exif.as_slice()));
                    assert!(reloaded.icc_profile.is_some());
                    for (actual, expected) in
                        reloaded.buffer.pixels().iter().zip(decoded.buffer.pixels())
                    {
                        for (a, e) in actual.iter().zip(expected) {
                            assert!(
                                (a.to_f32() - e.to_f32()).abs() < 0.002,
                                "{format:?}, orientation {orientation}: {actual:?} vs {expected:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn absent_or_unrecognized_orientation_is_preserved_and_metadata_is_bounded() {
        for exif in [source_exif(true, None), source_exif(false, Some(9))] {
            let mut output = Vec::new();
            write_srgb16_png_with_exif(&mut output, 1, 1, Some(&exif), [Ok(vec![[f16::ONE; 4]])])
                .unwrap();
            let mut decoder = image::codecs::png::PngDecoder::new(Cursor::new(output)).unwrap();
            assert_eq!(decoder.exif_metadata().unwrap(), Some(exif));
        }
        let mut output = Vec::new();
        let oversized = vec![0; crate::MAX_METADATA_BYTES + 1];
        let error = write_srgb16_png_with_exif(
            &mut output,
            1,
            1,
            Some(&oversized),
            [Ok(vec![[f16::ONE; 4]])],
        )
        .unwrap_err();
        assert!(error.to_string().contains("EXIF metadata exceeds"));
        assert!(output.is_empty());
    }

    #[test]
    fn empty_exif_is_omitted_and_malformed_exif_fails_before_writing() {
        let mut output = Vec::new();
        write_srgb16_png_with_exif(&mut output, 1, 1, Some(&[]), [Ok(vec![[f16::ONE; 4]])])
            .unwrap();
        let mut decoder = image::codecs::png::PngDecoder::new(Cursor::new(output)).unwrap();
        assert!(decoder.exif_metadata().unwrap().is_none());
        image::DynamicImage::from_decoder(decoder).unwrap();

        let valid = source_exif(true, Some(6));
        let mut bad_ifd_offset = valid.clone();
        bad_ifd_offset[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut bad_value_offset = valid.clone();
        bad_value_offset[18..22].copy_from_slice(&u32::MAX.to_le_bytes());
        for bytes in [
            vec![0; 7],
            valid[..20].to_vec(),
            bad_ifd_offset,
            bad_value_offset,
        ] {
            let mut output = Vec::new();
            let error = write_srgb16_png_with_exif(
                &mut output,
                1,
                1,
                Some(&bytes),
                [Ok(vec![[f16::ONE; 4]])],
            )
            .unwrap_err();
            assert!(error.to_string().contains("Invalid source EXIF metadata"));
            assert!(
                output.is_empty(),
                "invalid EXIF must fail before PNG header writes"
            );
        }
    }

    #[test]
    fn png_roundtrip_preserves_depth_color_alpha_and_output_profile() {
        let pixel = [0.25, 0.25, 0.25, 0.5].map(f16::from_f32);
        let mut output = Vec::new();
        write_srgb16_png(&mut output, 3, 2, [Ok(vec![pixel; 3]), Ok(vec![pixel; 3])]).unwrap();
        let mut decoder = image::codecs::png::PngDecoder::new(Cursor::new(&output)).unwrap();
        assert_eq!(decoder.dimensions(), (3, 2));
        assert_eq!(decoder.color_type(), image::ColorType::Rgba16);
        let icc = decoder.icc_profile().unwrap().unwrap();
        let actual_profile = ColorProfile::new_from_slice(&icc).unwrap();
        let mut expected_profile = ColorProfile::new_srgb();
        // Profile constructors stamp the current second. Verify the profile's
        // color definition without requiring two calls to share a clock tick.
        expected_profile.creation_date_time = actual_profile.creation_date_time;
        assert_eq!(icc, expected_profile.encode().unwrap());
        assert!(decoder.exif_metadata().unwrap().is_none());
        let rgba = image::DynamicImage::from_decoder(decoder)
            .unwrap()
            .into_rgba16();
        for p in rgba.pixels() {
            let expected = (linear_to_srgb(0.25) * 65535.0).round() as i32;
            for c in &p.0[..3] {
                assert!((i32::from(*c) - expected).abs() <= 1);
            }
            assert_eq!(p.0[3], 32768);
        }
    }
    #[test]
    fn png_rejects_bad_stream_lengths_and_propagates_producer_errors() {
        assert!(write_srgb16_png(Vec::new(), 1, 1, [Ok(vec![])]).is_err());
        assert!(write_srgb16_png(Vec::new(), 1, 1, [Ok(vec![[f16::ONE; 4]; 2])]).is_err());
        let error =
            write_srgb16_png(Vec::new(), 1, 1, [Err(anyhow::anyhow!("cancelled"))]).unwrap_err();
        assert_eq!(error.to_string(), "cancelled");
    }
}
