use anyhow::{Context, Result, ensure};
use filter_core::color::{linear_to_srgb, rec2020_to_linear_srgb, srgb_to_linear};
use half::f16;
use image::ImageEncoder;
use moxcms::ColorProfile;
use std::{
    borrow::Cow,
    io::{Seek, Write},
};

/// TIFF channel depth. Both modes preserve straight alpha.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TiffBitDepth {
    Eight,
    #[default]
    Sixteen,
}

/// Uncompressed TIFF settings. Dithering affects only 8-bit RGB, never alpha.
#[derive(Clone, Copy, Debug)]
pub struct TiffOptions {
    pub bit_depth: TiffBitDepth,
    pub dither_seed: Option<u64>,
}

impl Default for TiffOptions {
    fn default() -> Self {
        Self {
            bit_depth: TiffBitDepth::Sixteen,
            dither_seed: Some(0),
        }
    }
}

/// Stream oriented straight linear Rec.2020 bands as uncompressed sRGB TIFF.
/// Embeds output ICC and marks alpha as unassociated. Source EXIF is omitted.
/// Pixel payload is limited to 512 MiB. Conversion buffers target 1 MiB strips
/// (or one wider scanline), in addition to the current input band. Sixteen-bit
/// output is undithered. Caller owns atomic publication; output must be seekable
/// and positioned at offset zero.
pub fn write_srgb_tiff(
    output: impl Write + Seek,
    width: u32,
    height: u32,
    options: TiffOptions,
    bands: impl IntoIterator<Item = Result<Vec<[f16; 4]>>>,
) -> Result<()> {
    write_srgb_tiff_cancellable(output, width, height, options, bands, || false)
}

/// Like [`write_srgb_tiff`], checking cancellation before output, between bands,
/// at most every 4096 converted pixels, before strip writes and finalization.
/// Caller must also check output writes/seeks and publication for cancellation.
pub fn write_srgb_tiff_cancellable(
    mut output: impl Write + Seek,
    width: u32,
    height: u32,
    options: TiffOptions,
    bands: impl IntoIterator<Item = Result<Vec<[f16; 4]>>>,
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<()> {
    ensure!(!is_cancelled(), "Export cancelled");
    ensure!(width > 0 && height > 0, "Invalid TIFF dimensions");
    let bytes_per_pixel = match options.bit_depth {
        TiffBitDepth::Eight => 4_u64,
        TiffBitDepth::Sixteen => 8,
    };
    ensure!(
        (u64::from(width) * u64::from(height))
            .checked_mul(bytes_per_pixel)
            .is_some_and(|bytes| bytes <= 512 * 1024 * 1024),
        "TIFF pixel payload exceeds the 512 MiB limit"
    );
    let rows_per_strip = (1024 * 1024 / (u64::from(width) * bytes_per_pixel))
        .max(1)
        .min(u64::from(height)) as u32;
    ensure!(
        output.stream_position()? == 0,
        "TIFF output must start at offset zero"
    );
    match options.bit_depth {
        TiffBitDepth::Eight => write_tiff_strips::<tiff::encoder::colortype::RGB8>(
            &mut output,
            (width, height),
            rows_per_strip,
            bands,
            &mut is_cancelled,
            |pixel, index| straight_srgb8_pixel(pixel, options.dither_seed, index),
        )?,
        TiffBitDepth::Sixteen => write_tiff_strips::<tiff::encoder::colortype::RGB16>(
            &mut output,
            (width, height),
            rows_per_strip,
            bands,
            &mut is_cancelled,
            |pixel, _| straight_srgb16_pixel(pixel),
        )?,
    }
    output.flush()?;
    Ok(())
}

fn write_tiff_strips<C: tiff::encoder::colortype::ColorType>(
    output: &mut (impl Write + Seek),
    (width, height): (u32, u32),
    rows_per_strip: u32,
    bands: impl IntoIterator<Item = Result<Vec<[f16; 4]>>>,
    is_cancelled: &mut impl FnMut() -> bool,
    convert: impl Fn([f16; 4], u64) -> [C::Inner; 4],
) -> Result<()>
where
    [C::Inner]: tiff::encoder::TiffValue,
{
    // RGB + an explicit extra sample avoids ambiguous alpha interpretation.
    let icc = ColorProfile::new_srgb().encode()?;
    let mut strip = Vec::new();
    strip
        .try_reserve_exact(width as usize * rows_per_strip as usize * 4)
        .context("Could not allocate TIFF strip buffer")?;
    let mut encoder = tiff::encoder::TiffEncoder::new(output)?;
    let mut image = encoder.new_image::<C>(width, height)?;
    image.extra_samples(&[tiff::tags::ExtraSamples::UnassociatedAlpha])?;
    image.rows_per_strip(rows_per_strip)?;
    image
        .encoder()
        .write_tag(tiff::tags::Tag::IccProfile, icc.as_slice())?;
    image
        .encoder()
        .write_tag(tiff::tags::Tag::Orientation, 1_u16)?;
    let count = u64::from(width) * u64::from(height);
    let mut index = 0;
    let mut strip_samples = image.next_strip_sample_count();
    for band in bands {
        ensure!(!is_cancelled(), "Export cancelled");
        let band = band?;
        ensure!(
            band.len() as u64 <= count - index,
            "Too many pixels for TIFF dimensions"
        );
        for chunk in band.chunks(4096) {
            ensure!(!is_cancelled(), "Export cancelled");
            for &pixel in chunk {
                strip.extend(convert(pixel, index));
                index += 1;
                if strip.len() as u64 == strip_samples {
                    ensure!(!is_cancelled(), "Export cancelled");
                    image.write_strip(&strip)?;
                    strip.clear();
                    strip_samples = image.next_strip_sample_count();
                }
            }
        }
    }
    ensure!(index == count, "Not enough pixels for TIFF dimensions");
    ensure!(!is_cancelled(), "Export cancelled");
    image.finish()?;
    Ok(())
}

fn straight_srgb16_pixel(pixel: [f16; 4]) -> [u16; 4] {
    let [r, g, b, a] = pixel.map(f16::to_f32);
    let rgb = rec2020_to_linear_srgb([r, g, b]).map(|c| linear_to_srgb(c).clamp(0.0, 1.0));
    [rgb[0], rgb[1], rgb[2], a.clamp(0.0, 1.0)].map(|channel| (channel * 65535.0).round() as u16)
}

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
                            for channel in straight_srgb16_pixel(*pixel) {
                                bytes.extend_from_slice(&channel.to_be_bytes());
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
#[path = "../tests/unit/export_tests.rs"]
mod tests;
