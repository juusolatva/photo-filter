//! Image and preset file I/O, independent of GPU and GUI.
use anyhow::{Context, Result, ensure};
use filter_core::ImageBuffer;
use half::f16;
use image::{ImageDecoder, ImageReader, metadata::Orientation};
use moxcms::{ColorProfile, DataColorSpace, Layout, ToneReprCurve, TransformOptions};
use std::path::Path;

mod export;
pub mod preset;
pub use export::{
    JpegOptions, PngBitDepth, PngOptions, TiffBitDepth, TiffOptions, WebPOptions, write_srgb_png,
    write_srgb_png_cancellable, write_srgb_tiff, write_srgb_tiff_cancellable, write_srgb8_jpeg,
    write_srgb8_jpeg_cancellable, write_srgb8_webp, write_srgb8_webp_cancellable, write_srgb16_png,
    write_srgb16_png_with_exif,
};

pub struct DecodedImage {
    pub buffer: ImageBuffer,
    pub source_orientation: Orientation,
    pub icc_profile: Option<Vec<u8>>,
    pub exif_bytes: Option<Vec<u8>>,
}

/// A conservative 2 GiB peak working budget (source decode, float conversion, CMS and f16 storage).
const MAX_METADATA_BYTES: usize = 16 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_PIXELS: u64 = (2 * 1024 * 1024 * 1024 - 2 * MAX_METADATA_BYTES as u64) / 48;

pub fn decode(path: &Path) -> Result<DecodedImage> {
    let file = std::fs::File::open(path)?;
    ensure!(
        file.metadata()?.len() <= MAX_FILE_BYTES,
        "Input file exceeds the initial 512 MiB limit"
    );
    // Pixel decoding also uses ImageReader's default 512 MiB allocation limit.
    // This is a working-set estimate, not an OS-enforced RSS limit.
    let reader = ImageReader::new(std::io::BufReader::new(file)).with_guessed_format()?;
    // image's TIFF set_limits caps tag allocations at the pixel buffer size;
    // on small images that silently hides larger ICC profiles. Read metadata
    // with the TIFF decoder's initial limits, then apply pixel limits below.
    let mut decoder: Box<dyn ImageDecoder> = if reader.format() == Some(image::ImageFormat::Tiff) {
        Box::new(
            image::codecs::tiff::TiffDecoder::new(reader.into_inner())
                .context("Unsupported or invalid image")?,
        )
    } else {
        Box::new(
            reader
                .into_decoder()
                .context("Unsupported or invalid image")?,
        )
    };
    let (w, h) = decoder.dimensions();
    ensure!(
        w > 0 && h > 0 && u64::from(w) * u64::from(h) <= MAX_PIXELS,
        "Image exceeds the initial 2 GiB working-memory budget (maximum {MAX_PIXELS} pixels)"
    );
    let icc_profile = decoder.icc_profile()?;
    ensure!(
        icc_profile
            .as_ref()
            .is_none_or(|b| b.len() <= MAX_METADATA_BYTES),
        "ICC metadata exceeds the 16 MiB limit"
    );
    let exif_bytes = decoder.exif_metadata()?;
    ensure!(
        exif_bytes
            .as_ref()
            .is_none_or(|b| b.len() <= MAX_METADATA_BYTES),
        "EXIF metadata exceeds the 16 MiB limit"
    );
    let source_orientation = decoder.orientation()?;
    decoder.set_limits(image::Limits::default())?;
    let mut image = image::DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(source_orientation);
    let rgba = image.into_rgba32f();
    let source = match &icc_profile {
        Some(bytes) => {
            ColorProfile::new_from_slice(bytes).context("Invalid embedded ICC profile")?
        }
        None => ColorProfile::new_srgb(),
    };
    if source.lut_a_to_b_perceptual.is_some()
        || source.lut_a_to_b_colorimetric.is_some()
        || source.lut_a_to_b_saturation.is_some()
    {
        eprintln!("ICC LUT profile: color conversion may clamp extended-range values");
    }
    let pixels = convert_to_working(rgba.as_raw(), &source)?;
    Ok(DecodedImage {
        buffer: ImageBuffer::new(rgba.width(), rgba.height(), pixels)
            .map_err(anyhow::Error::msg)?,
        source_orientation,
        icc_profile,
        exif_bytes,
    })
}

fn convert_to_working(rgba: &[f32], source: &ColorProfile) -> Result<Vec<[f16; 4]>> {
    let mut destination = ColorProfile::new_bt2020();
    let linear = ToneReprCurve::Parametric(vec![1.0]);
    destination.red_trc = Some(linear.clone());
    destination.green_trc = Some(linear.clone());
    destination.blue_trc = Some(linear);
    let input_layout = match source.color_space {
        DataColorSpace::Rgb => Layout::Rgba,
        DataColorSpace::Gray => Layout::Gray,
        _ => anyhow::bail!("Unsupported ICC color space: {:?}", source.color_space),
    };
    let transform = source.create_transform_f32(
        input_layout,
        &destination,
        Layout::Rgba,
        TransformOptions {
            allow_extended_range_rgb_xyz: true,
            ..Default::default()
        },
    )?;
    // Transform in bounded chunks rather than allocating a second full f32 image.
    let mut pixels = Vec::with_capacity(rgba.len() / 4);
    for chunk in rgba.chunks(4096 * 4) {
        let mut output = vec![0.0; chunk.len()];
        if input_layout == Layout::Gray {
            // Decoded gray samples are replicated into RGB; CMS must still see one channel.
            let gray: Vec<f32> = chunk.as_chunks::<4>().0.iter().map(|p| p[0]).collect();
            transform.transform(&gray, &mut output)?;
        } else {
            transform.transform(chunk, &mut output)?;
        }
        for (rgb, original) in output
            .as_chunks::<4>()
            .0
            .iter()
            .zip(chunk.as_chunks::<4>().0)
        {
            pixels.push([
                f16::from_f32(rgb[0]),
                f16::from_f32(rgb[1]),
                f16::from_f32(rgb[2]),
                f16::from_f32(original[3]),
            ]);
        }
    }
    Ok(pixels)
}

#[cfg(test)]
#[path = "../tests/unit/lib_tests.rs"]
mod tests;
