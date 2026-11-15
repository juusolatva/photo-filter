//! Standard-format decoder boundary, independent of GPU and GUI.
use anyhow::{Context, Result, ensure};
use filter_core::ImageBuffer;
use half::f16;
use image::{ImageDecoder, ImageReader, metadata::Orientation};
use moxcms::{ColorProfile, Layout, ToneReprCurve, TransformOptions};
use std::path::Path;

pub struct DecodedImage {
    pub buffer: ImageBuffer,
    pub source_orientation: Orientation,
    pub icc_profile: Option<Vec<u8>>,
    pub exif_bytes: Option<Vec<u8>>,
}

/// A conservative 2 GiB peak working budget (source decode, float conversion, CMS and f16 storage).
const MAX_PIXELS: u64 = 2 * 1024 * 1024 * 1024 / 48;

pub fn decode(path: &Path) -> Result<DecodedImage> {
    let reader = ImageReader::open(path)?.with_guessed_format()?;
    let mut decoder = reader
        .into_decoder()
        .context("Unsupported or invalid image")?;
    let (w, h) = decoder.dimensions();
    ensure!(
        w > 0 && h > 0 && u64::from(w) * u64::from(h) <= MAX_PIXELS,
        "Image exceeds the initial 2 GiB working-memory budget (maximum {MAX_PIXELS} pixels)"
    );
    let icc_profile = decoder.icc_profile()?;
    let exif_bytes = decoder.exif_metadata()?;
    let source_orientation = decoder.orientation()?;
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
    let transform = source.create_transform_f32(
        Layout::Rgba,
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
        transform.transform(chunk, &mut output)?;
        for (rgb, original) in output.chunks_exact(4).zip(chunk.chunks_exact(4)) {
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
mod tests {
    use super::*;
    #[test]
    fn png_decode_applies_orientation_once_and_preserves_metadata() {
        use image::ImageEncoder;
        // Little-endian TIFF IFD0: one SHORT Orientation=6 entry (90 degrees clockwise).
        let exif = vec![
            73, 73, 42, 0, 8, 0, 0, 0, 1, 0, 18, 1, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
        ];
        let icc = ColorProfile::new_srgb().encode().unwrap();
        let path = std::env::temp_dir().join(format!(
            "photo-filter-orientation-{}-{}.png",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let file = std::fs::File::create(&path).unwrap();
        let mut encoder = image::codecs::png::PngEncoder::new(file);
        encoder.set_exif_metadata(exif).unwrap();
        encoder.set_icc_profile(icc.clone()).unwrap();
        encoder
            .write_image(
                &[255, 0, 0, 255, 0, 0, 255, 255],
                2,
                1,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        let decoded = decode(&path);
        std::fs::remove_file(path).unwrap();
        let decoded = decoded.unwrap();
        assert_eq!((decoded.buffer.width(), decoded.buffer.height()), (1, 2));
        assert_eq!(decoded.source_orientation, Orientation::Rotate90);
        assert_eq!(decoded.icc_profile, Some(icc));
        assert!(decoded.exif_bytes.is_some());
        let red = decoded.buffer.pixels()[0].map(f16::to_f32);
        let blue = decoded.buffer.pixels()[1].map(f16::to_f32);
        assert!(red[0] > red[2] && blue[2] > blue[0]);
    }
    #[test]
    fn srgb_white_and_alpha_survive() {
        let p = convert_to_working(&[1.0, 1.0, 1.0, 0.25], &ColorProfile::new_srgb()).unwrap()[0]
            .map(f16::to_f32);
        for c in &p[..3] {
            assert!((c - 1.0).abs() < 0.002);
        }
        assert_eq!(p[3], 0.25);
    }
    #[test]
    fn p3_red_stays_outside_srgb() {
        let p = convert_to_working(&[1.0, 0.0, 0.0, 1.0], &ColorProfile::new_display_p3()).unwrap()
            [0]
        .map(f16::to_f32);
        let srgb = filter_core::color::rec2020_to_linear_srgb([p[0], p[1], p[2]]);
        assert!(srgb[0] > 1.1 && srgb[1] < 0.0);
        assert!(p[..3].iter().all(|x| *x >= -0.002), "{p:?}");
    }
}
