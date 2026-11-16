use super::*;
#[test]
fn small_tiff_retains_wide_gamut_icc_before_applying_pixel_limits() {
    use image::ImageEncoder;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("p3.tiff");
    let icc = ColorProfile::new_display_p3().encode().unwrap();
    let mut encoder = image::codecs::tiff::TiffEncoder::new(std::fs::File::create(&path).unwrap());
    encoder.set_icc_profile(icc.clone()).unwrap();
    encoder
        .write_image(&[255, 0, 0], 1, 1, image::ExtendedColorType::Rgb8)
        .unwrap();
    let decoded = decode(&path).unwrap();
    assert_eq!(decoded.icc_profile, Some(icc));
    assert_eq!(decoded.source_orientation, Orientation::NoTransforms);
    let p = decoded.buffer.pixels()[0].map(f16::to_f32);
    let srgb = filter_core::color::rec2020_to_linear_srgb([p[0], p[1], p[2]]);
    assert!(srgb[0] > 1.1 && srgb[1] < 0.0, "{srgb:?}");
}
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
    let path = std::env::temp_dir().join(format!("photo-filter-gray-{}.png", std::process::id()));
    let mut encoder = image::codecs::png::PngEncoder::new(std::fs::File::create(&path).unwrap());
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
    let mut encoder = image::codecs::png::PngEncoder::new(std::fs::File::create(&path).unwrap());
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
    let p = convert_to_working(&[1.0, 0.0, 0.0, 1.0], &ColorProfile::new_display_p3()).unwrap()[0]
        .map(f16::to_f32);
    let srgb = filter_core::color::rec2020_to_linear_srgb([p[0], p[1], p[2]]);
    assert!(srgb[0] > 1.1 && srgb[1] < 0.0);
    assert!(p[..3].iter().all(|x| *x >= -0.002), "{p:?}");
}
