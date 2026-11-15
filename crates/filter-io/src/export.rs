use anyhow::{Context, Result, ensure};
use filter_core::color::{linear_to_srgb, rec2020_to_linear_srgb};
use half::f16;
use moxcms::ColorProfile;
use std::{borrow::Cow, io::Write};

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
    ensure!(width > 0 && height > 0, "Invalid PNG dimensions");
    ensure!(
        exif.is_none_or(|bytes| bytes.len() <= crate::MAX_METADATA_BYTES),
        "EXIF metadata exceeds the 16 MiB limit"
    );
    let exif = exif.filter(|bytes| !bytes.is_empty());
    if let Some(bytes) = exif {
        exif::Reader::new()
            .read_raw(bytes.to_vec())
            .context("Invalid source EXIF metadata")?;
    }
    let mut info = png::Info::with_size(width, height);
    info.color_type = png::ColorType::Rgba;
    info.bit_depth = png::BitDepth::Sixteen;
    info.icc_profile = Some(Cow::Owned(ColorProfile::new_srgb().encode()?));
    info.exif_metadata = exif.map(|bytes| {
        let mut bytes = bytes.to_vec();
        let _ = image::metadata::Orientation::remove_from_exif_chunk(&mut bytes);
        Cow::Owned(bytes)
    });
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
