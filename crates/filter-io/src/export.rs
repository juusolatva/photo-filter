use anyhow::{Result, ensure};
use filter_core::color::{linear_to_srgb, rec2020_to_linear_srgb};
use half::f16;
use moxcms::ColorProfile;
use std::{borrow::Cow, io::Write};

/// Stream row-major straight linear Rec.2020 pixels to a 16-bit RGBA sRGB PNG.
/// Quantization clips to the initial sRGB output gamut, matching the viewport.
/// Embeds the output ICC profile; source EXIF is deliberately omitted because the
/// decoder has already applied its orientation. Caller owns atomic file publication.
pub fn write_srgb16_png(
    output: impl Write,
    width: u32,
    height: u32,
    bands: impl IntoIterator<Item = Result<Vec<[f16; 4]>>>,
) -> Result<()> {
    ensure!(width > 0 && height > 0, "Invalid PNG dimensions");
    let mut info = png::Info::with_size(width, height);
    info.color_type = png::ColorType::Rgba;
    info.bit_depth = png::BitDepth::Sixteen;
    info.icc_profile = Some(Cow::Owned(ColorProfile::new_srgb().encode()?));
    let mut writer = png::Encoder::with_info(output, info)?.write_header()?;
    let mut remaining = u64::from(width) * u64::from(height);
    {
        let mut stream = writer.stream_writer()?;
        let mut bytes = Vec::with_capacity(4096 * 8);
        for band in bands {
            let band = band?;
            ensure!(
                band.len() as u64 <= remaining,
                "Too many pixels for PNG dimensions"
            );
            remaining -= band.len() as u64;
            for chunk in band.chunks(4096) {
                bytes.clear();
                for pixel in chunk {
                    let [r, g, b, a] = pixel.map(f16::to_f32);
                    for channel in rec2020_to_linear_srgb([r, g, b])
                        .map(|c| linear_to_srgb(c).clamp(0.0, 1.0))
                        .into_iter()
                        .chain([a.clamp(0.0, 1.0)])
                    {
                        bytes
                            .extend_from_slice(&((channel * 65535.0).round() as u16).to_be_bytes());
                    }
                }
                stream.write_all(&bytes)?;
            }
        }
        ensure!(remaining == 0, "Not enough pixels for PNG dimensions");
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
    #[test]
    fn png_roundtrip_preserves_depth_color_alpha_and_output_profile() {
        let pixel = [0.25, 0.25, 0.25, 0.5].map(f16::from_f32);
        let mut output = Vec::new();
        write_srgb16_png(&mut output, 3, 2, [Ok(vec![pixel; 3]), Ok(vec![pixel; 3])]).unwrap();
        let mut decoder = image::codecs::png::PngDecoder::new(Cursor::new(&output)).unwrap();
        assert_eq!(decoder.dimensions(), (3, 2));
        assert_eq!(decoder.color_type(), image::ColorType::Rgba16);
        assert_eq!(
            decoder.icc_profile().unwrap().unwrap(),
            ColorProfile::new_srgb().encode().unwrap()
        );
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
