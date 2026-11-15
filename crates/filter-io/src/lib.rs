//! Standard-format decoder boundary, independent of GPU and GUI.
use anyhow::{Context, Result, ensure};
use filter_core::ImageBuffer;
use half::f16;
use image::{ImageDecoder, ImageReader, metadata::Orientation};
use moxcms::{ColorProfile, DataColorSpace, Layout, ToneReprCurve, TransformOptions};
use std::path::Path;

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
    // ImageReader also applies its default 512 MiB decoder allocation limit.
    // This is a working-set estimate, not an OS-enforced RSS limit.
    let reader = ImageReader::new(std::io::BufReader::new(file)).with_guessed_format()?;
    let mut decoder = reader
        .into_decoder()
        .context("Unsupported or invalid image")?;
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
    fn tagged_grayscale_png_opens_in_working_space() {
        use image::ImageEncoder;
        let path =
            std::env::temp_dir().join(format!("photo-filter-gray-{}.png", std::process::id()));
        let mut encoder =
            image::codecs::png::PngEncoder::new(std::fs::File::create(&path).unwrap());
        encoder
            .set_icc_profile(ColorProfile::new_gray_with_gamma(2.2).encode().unwrap())
            .unwrap();
        encoder
            .write_image(&[128, 64], 1, 1, image::ExtendedColorType::La8)
            .unwrap();
        let decoded = decode(&path);
        std::fs::remove_file(path).unwrap();
        let p = decoded.unwrap().buffer.pixels()[0].map(f16::to_f32);
        let expected = (128.0_f32 / 255.0).powf(2.2);
        for c in &p[..3] {
            assert!((c - expected).abs() < 0.005, "{p:?}");
        }
        assert!((p[3] - 64.0 / 255.0).abs() < 0.001);
    }
    #[test]
    fn oversized_metadata_is_rejected() {
        use image::ImageEncoder;
        let path =
            std::env::temp_dir().join(format!("photo-filter-metadata-{}.png", std::process::id()));
        let mut encoder =
            image::codecs::png::PngEncoder::new(std::fs::File::create(&path).unwrap());
        encoder
            .set_exif_metadata(vec![0; MAX_METADATA_BYTES + 1])
            .unwrap();
        encoder
            .write_image(&[0, 0, 0, 255], 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        let error = decode(&path).err().unwrap();
        std::fs::remove_file(path).unwrap();
        assert!(
            error.to_string().contains("EXIF metadata exceeds"),
            "{error:#}"
        );
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
