use super::{
    AtomicBool, AtomicU64, BasicAdjustments, CancelWriter, ExportFormat, ExportResize, Instant,
    JpegOptions, Ordering, PngBitDepth, PngOptions, ResizeBands, Seek, TiffBitDepth, TiffOptions,
    TilePlan, WebPOptions, export_image, publish_export,
};
use std::io::Write;

#[test]
fn export_requires_an_extension_matching_the_selected_format() {
    assert!(
        ExportFormat::Png(PngOptions::default())
            .validate_path(std::path::Path::new("photo.png"))
            .is_ok()
    );
    assert!(
        ExportFormat::Png(PngOptions::default())
            .validate_path(std::path::Path::new("photo.PNG"))
            .is_ok()
    );
    assert!(
        ExportFormat::Png(PngOptions::default())
            .validate_path(std::path::Path::new("photo.jpg"))
            .is_err()
    );
    assert!(
        ExportFormat::Png(PngOptions::default())
            .validate_path(std::path::Path::new("photo"))
            .is_err()
    );
}

#[test]
fn jpeg_accepts_both_extensions_case_insensitively() {
    let format = ExportFormat::Jpeg(JpegOptions::default());
    for path in ["photo.jpg", "photo.JPEG", "photo.JPG", "photo.jpeg"] {
        assert!(format.validate_path(std::path::Path::new(path)).is_ok());
    }
    for path in ["photo.png", "photo", "photo.webp"] {
        assert!(format.validate_path(std::path::Path::new(path)).is_err());
    }
}

#[test]
fn webp_requires_its_extension_case_insensitively() {
    let format = ExportFormat::WebP(WebPOptions::default());
    assert_eq!(format.extension(), "webp");
    assert_eq!(format.label(), "Lossless WebP");
    for path in ["photo.webp", "photo.WEBP"] {
        assert!(format.validate_path(std::path::Path::new(path)).is_ok());
    }
    for path in ["photo.png", "photo.jpg", "photo"] {
        assert!(format.validate_path(std::path::Path::new(path)).is_err());
    }
}

#[test]
fn tiff_accepts_both_extensions_case_insensitively() {
    for bit_depth in [TiffBitDepth::Eight, TiffBitDepth::Sixteen] {
        let format = ExportFormat::Tiff(TiffOptions {
            bit_depth,
            ..Default::default()
        });
        assert_eq!(format.extension(), "tiff");
        assert!(format.label().contains("TIFF"));
        for path in ["photo.tif", "photo.TIFF", "photo.TIF", "photo.tiff"] {
            assert!(format.validate_path(std::path::Path::new(path)).is_ok());
        }
        for path in ["photo.png", "photo.jpg", "photo.webp", "photo"] {
            assert!(format.validate_path(std::path::Path::new(path)).is_err());
        }
    }
}

#[test]
fn cancellation_during_encoding_preserves_destination() {
    struct CancelAfterWrite<'a> {
        file: &'a mut std::fs::File,
        cancel: &'a AtomicBool,
    }
    impl Write for CancelAfterWrite<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let written = self.file.write(bytes)?;
            self.cancel.store(true, Ordering::Relaxed);
            Ok(written)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.file.flush()
        }
    }
    impl Seek for CancelAfterWrite<'_> {
        fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
            self.file.seek(position)
        }
    }
    let directory = tempfile::tempdir().unwrap();
    for format in [
        ExportFormat::Png(PngOptions {
            bit_depth: PngBitDepth::Eight,
            ..Default::default()
        }),
        ExportFormat::Png(PngOptions::default()),
        ExportFormat::Jpeg(JpegOptions::default()),
        ExportFormat::WebP(WebPOptions::default()),
        ExportFormat::Tiff(TiffOptions {
            bit_depth: TiffBitDepth::Eight,
            ..Default::default()
        }),
        ExportFormat::Tiff(TiffOptions::default()),
    ] {
        let path = directory
            .path()
            .join(format!("edited.{}", format.extension()));
        std::fs::write(&path, b"original").unwrap();
        let cancel = AtomicBool::new(false);
        let result = publish_export(&path, &cancel, |file| {
            let output = CancelWriter {
                output: CancelAfterWrite {
                    file,
                    cancel: &cancel,
                },
                cancel: &cancel,
            };
            let bands = [Ok(vec![[half::f16::ONE; 4]; 17 * 9])];
            match format {
                ExportFormat::Png(options) => {
                    filter_io::write_srgb_png(output, 17, 9, None, options, bands)
                }
                ExportFormat::Jpeg(options) => {
                    filter_io::write_srgb8_jpeg(output, 17, 9, None, options, bands)
                }
                ExportFormat::WebP(options) => {
                    filter_io::write_srgb8_webp(output, 17, 9, None, options, bands)
                }
                ExportFormat::Tiff(options) => {
                    filter_io::write_srgb_tiff(output, 17, 9, options, bands)
                }
            }
        });
        assert!(result.is_err());
        assert!(cancel.load(Ordering::Relaxed));
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
    }
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 4);
}

#[test]
fn tiff_stream_failures_and_conversion_cancellation_preserve_destination() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("edited.tiff");
    for bit_depth in [TiffBitDepth::Eight, TiffBitDepth::Sixteen] {
        for failure in 0..4 {
            std::fs::write(&path, b"original").unwrap();
            let cancel = AtomicBool::new(false);
            let result = publish_export(&path, &cancel, |file| {
                let bands = match failure {
                    0 => vec![
                        Ok(vec![[half::f16::ONE; 4]; 256 * 64]),
                        Err(anyhow::anyhow!("producer failed")),
                    ],
                    1 => vec![Ok(vec![[half::f16::ONE; 4]; 17])],
                    2 => vec![Ok(vec![[half::f16::ONE; 4]; 256 * 64 + 1])],
                    _ => vec![Ok(vec![[half::f16::ONE; 4]; 256 * 64])],
                };
                let mut checks = 0;
                filter_io::write_srgb_tiff_cancellable(
                    file,
                    256,
                    64,
                    TiffOptions {
                        bit_depth,
                        ..Default::default()
                    },
                    bands,
                    || {
                        checks += 1;
                        failure == 3 && checks >= 4
                    },
                )
            });
            assert!(result.is_err());
            assert_eq!(std::fs::read(&path).unwrap(), b"original");
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        }
    }
}

#[test]
#[cfg(unix)]
fn export_uses_creation_defaults_and_preserves_existing_unix_mode() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let reference = directory.path().join("normal-file");
    std::fs::write(&reference, b"normal creation").unwrap();
    let path = directory.path().join("edited.png");
    let cancel = AtomicBool::new(false);
    publish_export(&path, &cancel, |file| {
        file.write_all(b"new")?;
        Ok(())
    })
    .unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        std::fs::metadata(reference).unwrap().permissions().mode() & 0o777
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    publish_export(&path, &cancel, |file| {
        file.write_all(b"replacement")?;
        Ok(())
    })
    .unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o640
    );
}

#[test]
fn failed_or_cancelled_export_preserves_destination_and_cleans_temp_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("edited.png");
    std::fs::write(&path, b"original").unwrap();
    let cancel = AtomicBool::new(false);
    let result = publish_export(&path, &cancel, |file| {
        file.write_all(b"incomplete")?;
        anyhow::bail!("simulated encode failure");
    });
    assert!(result.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"original");
    let result = publish_export(&path, &cancel, |file| {
        filter_io::write_srgb16_png_with_exif(
            file,
            1,
            1,
            Some(&[0; 7]),
            [Ok(vec![[half::f16::ONE; 4]])],
        )
    });
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Invalid source EXIF metadata")
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"original");
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    let result = publish_export(&path, &cancel, |file| {
        file.write_all(b"finished but cancelled")?;
        cancel.store(true, Ordering::Relaxed);
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"original");
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    cancel.store(false, Ordering::Relaxed);
    publish_export(&path, &cancel, |file| {
        file.write_all(b"complete")?;
        Ok(())
    })
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"complete");
}

#[test]
fn resize_failures_and_cancellation_preserve_destinations_in_every_format() {
    let directory = tempfile::tempdir().unwrap();
    for format in [
        ExportFormat::Png(PngOptions::default()),
        ExportFormat::Png(PngOptions {
            bit_depth: PngBitDepth::Eight,
            ..Default::default()
        }),
        ExportFormat::Jpeg(JpegOptions::default()),
        ExportFormat::WebP(WebPOptions::default()),
        ExportFormat::Tiff(TiffOptions::default()),
        ExportFormat::Tiff(TiffOptions {
            bit_depth: TiffBitDepth::Eight,
            ..Default::default()
        }),
    ] {
        for failure in 0..7 {
            let path = directory
                .path()
                .join(format!("resized.{}", format.extension()));
            std::fs::write(&path, b"original").unwrap();
            let cancel = AtomicBool::new(false);
            let mut index = 0;
            let bands = std::iter::from_fn(|| {
                index += 1;
                match index {
                    1 if failure == 5 => Some(Ok(Vec::new())),
                    1 => Some(Ok(vec![[half::f16::ONE; 4]; 8])),
                    2 if failure == 0 => Some(Err(anyhow::anyhow!("render failed"))),
                    2 if failure == 1 => None,
                    2 if failure == 2 => {
                        cancel.store(true, Ordering::Relaxed);
                        Some(Ok(vec![[half::f16::ONE; 4]; 8]))
                    }
                    2 if failure == 4 => Some(Ok(Vec::new())),
                    2 => Some(Ok(vec![[half::f16::ONE; 4]; 8])),
                    3 if failure == 3 => Some(Err(anyhow::anyhow!("late render failure"))),
                    3 if failure == 6 => Some(Ok(Vec::new())),
                    _ => None,
                }
            });
            let result = publish_export(&path, &cancel, |file| {
                let bands = ResizeBands::new(bands, (4, 4), (2, 2), &cancel)?;
                match format {
                    ExportFormat::Png(options) => {
                        filter_io::write_srgb_png(file, 2, 2, None, options, bands)
                    }
                    ExportFormat::Jpeg(options) => {
                        filter_io::write_srgb8_jpeg(file, 2, 2, None, options, bands)
                    }
                    ExportFormat::WebP(options) => {
                        filter_io::write_srgb8_webp(file, 2, 2, None, options, bands)
                    }
                    ExportFormat::Tiff(options) => {
                        filter_io::write_srgb_tiff(file, 2, 2, options, bands)
                    }
                }
            });
            assert!(result.is_err(), "{format:?}, failure {failure}");
            assert_eq!(std::fs::read(&path).unwrap(), b"original");
        }
    }
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 4);
}

fn software_device(required_limits: wgpu::Limits) -> (wgpu::Device, wgpu::Queue) {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY)
                & wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                force_fallback_adapter: true,
                ..Default::default()
            })
            .await
            .expect("software adapter required");
        eprintln!("Software GPU: {:?}", adapter.get_info());
        adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_limits,
                ..Default::default()
            })
            .await
            .unwrap()
    })
}

/// One captured source, metadata and adjustment snapshot to export.
#[derive(Clone, Copy)]
struct ExportCase<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    source: &'a filter_core::ImageBuffer,
    exif: Option<&'a [u8]>,
    adjustments: &'a BasicAdjustments,
}
impl ExportCase<'_> {
    /// Export through a fresh tile plan and check every tile reported completion.
    fn run(
        &self,
        path: &std::path::Path,
        format: ExportFormat,
        tile_edge: u32,
        resize: ExportResize,
    ) {
        let (w, h) = (self.source.width(), self.source.height());
        let plan = TilePlan::new(w, h, &self.device.limits(), tile_edge).unwrap();
        let tiles = plan.tile_count();
        let completed = AtomicU64::new(0);
        export_image(
            self.device,
            self.queue,
            self.source,
            self.exif,
            self.adjustments,
            path,
            format,
            plan,
            &AtomicBool::new(false),
            &completed,
            resize,
        )
        .unwrap();
        assert_eq!(completed.load(Ordering::Relaxed), tiles);
    }
    /// CPU reference for the adjusted source at the renderer's half-float boundary.
    fn adjusted_source(&self) -> filter_core::ImageBuffer {
        filter_core::ImageBuffer::new(
            self.source.width(),
            self.source.height(),
            self.source
                .pixels()
                .iter()
                .map(|p| {
                    self.adjustments
                        .pixel(p.map(half::f16::to_f32))
                        .map(half::f16::from_f32)
                })
                .collect(),
        )
        .unwrap()
    }
}

/// Little-endian TIFF-structured EXIF with orientation 6 (rotate 90°).
const EXIF: [u8; 26] = [
    73, 73, 42, 0, 8, 0, 0, 0, 1, 0, 18, 1, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
];
/// Exports bake orientation into pixels, so retained EXIF says "normal".
const NORMALIZED_EXIF: [u8; 26] = {
    let mut exif = EXIF;
    exif[18] = 1;
    exif
};
const JPEG: JpegOptions = JpegOptions {
    quality: 100,
    background: [81, 142, 203],
    dither_seed: None,
};

/// Renderer output before encoding: adjusted, then stored as half floats.
fn working_pixel(adjustments: &BasicAdjustments, original: [half::f16; 4]) -> [f32; 4] {
    adjustments
        .pixel(original.map(half::f16::to_f32))
        .map(|v| half::f16::from_f32(v).to_f32())
}
/// Clipped, encoded sRGB in [0, 1] for a working Rec.2020 pixel.
fn encoded_srgb([r, g, b, _]: [f32; 4]) -> [f32; 3] {
    filter_core::color::rec2020_to_linear_srgb([r, g, b])
        .map(|c| filter_core::color::linear_to_srgb(c).clamp(0.0, 1.0))
}

fn assert_samples_close<T: Copy + Into<i32> + std::fmt::Debug>(
    actual: &[T],
    expected: &[T],
    tolerance: i32,
    context: &str,
) {
    assert_eq!(actual.len(), expected.len(), "{context}");
    for (i, (a, e)) in actual.iter().zip(expected).enumerate() {
        assert!(
            ((*a).into() - (*e).into()).abs() <= tolerance,
            "{context}: sample {i}: {a:?} vs {e:?}"
        );
    }
}

/// Decoded metadata: ICC always, EXIF only where the format retains it, and
/// orientation already applied.
fn assert_export_metadata(path: &std::path::Path, exif: Option<&[u8]>) {
    let reloaded = filter_io::decode(path).unwrap();
    assert!(reloaded.icc_profile.is_some());
    assert_eq!(reloaded.exif_bytes.as_deref(), exif);
    assert_eq!(
        reloaded.source_orientation,
        image::metadata::Orientation::NoTransforms
    );
}

fn png_reader(path: &std::path::Path) -> png::Reader<std::io::BufReader<std::fs::File>> {
    png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).unwrap()))
        .read_info()
        .unwrap()
}

/// 16-bit PNG matches the CPU reference within half an 8-bit step; alpha is exact.
fn assert_png16_matches_cpu(case: &ExportCase, path: &std::path::Path) {
    let mut reader = png_reader(path);
    assert!(reader.info().icc_profile.is_some());
    assert_eq!(
        reader.info().exif_metadata.as_deref(),
        Some(NORMALIZED_EXIF.as_slice())
    );
    assert_eq!(
        reader.output_color_type(),
        (png::ColorType::Rgba, png::BitDepth::Sixteen)
    );
    for row in case
        .source
        .pixels()
        .chunks_exact(case.source.width() as usize)
    {
        let actual = reader.next_row().unwrap().unwrap();
        for (original, bytes) in row.iter().zip(actual.data().as_chunks::<8>().0) {
            let working = working_pixel(case.adjustments, *original);
            let expected = encoded_srgb(working).map(|c| (c * 65535.0).round() as i32);
            let actual: [u16; 4] =
                std::array::from_fn(|c| u16::from_be_bytes([bytes[c * 2], bytes[c * 2 + 1]]));
            for c in 0..3 {
                // Half-float rounding can differ by one ULP across CPU/GPU.
                // Bound final sRGB error to less than half an 8-bit step.
                assert!(
                    (i32::from(actual[c]) - expected[c]).abs() <= 128,
                    "{} != {}",
                    actual[c],
                    expected[c]
                );
            }
            assert_eq!(actual[3], (working[3] * 65535.0).round() as u16);
        }
    }
    assert!(reader.next_row().unwrap().is_none());
    reader.finish().unwrap();
}

/// 16-bit TIFF reproduces the 16-bit PNG at different tile sizes, without EXIF.
fn check_tiff16_matches_png16(case: &ExportCase, dir: &std::path::Path, png16: &std::path::Path) {
    for tile_edge in [17, 31] {
        let tiff = dir.join("adjusted16.tiff");
        case.run(
            &tiff,
            ExportFormat::Tiff(TiffOptions::default()),
            tile_edge,
            ExportResize::Full,
        );
        assert_export_metadata(&tiff, None);
        assert_eq!(
            image::open(&tiff).unwrap().into_rgba16(),
            image::open(png16).unwrap().into_rgba16()
        );
    }
}

/// 8-bit outputs are identical across tile sizes, with and without dithering.
fn check_8bit_formats_agree_across_tiles(case: &ExportCase, dir: &std::path::Path) {
    for seed in [None, Some(0)] {
        let mut previous = None;
        for tile_edge in [17, 31] {
            let actual = check_8bit_formats_agree(case, dir, seed, tile_edge);
            if let Some(previous) = previous {
                assert_eq!(
                    actual, previous,
                    "tile boundaries must not affect dithering"
                );
            }
            previous = Some(actual);
        }
    }
}

/// PNG8, lossless WebP and TIFF8 quantize identically and match the CPU
/// reference. Returns the shared RGBA8 output.
fn check_8bit_formats_agree(
    case: &ExportCase,
    dir: &std::path::Path,
    seed: Option<u64>,
    tile_edge: u32,
) -> image::RgbaImage {
    let png = dir.join("adjusted8.png");
    let options = PngOptions {
        bit_depth: PngBitDepth::Eight,
        dither_seed: seed,
    };
    case.run(
        &png,
        ExportFormat::Png(options),
        tile_edge,
        ExportResize::Full,
    );
    assert_export_metadata(&png, Some(&NORMALIZED_EXIF));
    let actual = image::open(&png).unwrap();
    assert_eq!(actual.color(), image::ColorType::Rgba8);
    let actual = actual.into_rgba8();

    let webp = dir.join("adjusted.webp");
    let options = WebPOptions { dither_seed: seed };
    case.run(
        &webp,
        ExportFormat::WebP(options),
        tile_edge,
        ExportResize::Full,
    );
    assert_export_metadata(&webp, Some(&NORMALIZED_EXIF));
    assert_eq!(
        image::open(&webp).unwrap().into_rgba8(),
        actual,
        "lossless WebP must preserve the same quantized pixels as PNG8"
    );

    // TIFF deliberately omits EXIF, even when the captured bytes are malformed.
    let tiff = dir.join("adjusted8.tif");
    let malformed = ExportCase {
        exif: Some(b"invalid EXIF"),
        ..*case
    };
    let options = TiffOptions {
        bit_depth: TiffBitDepth::Eight,
        dither_seed: seed,
    };
    malformed.run(
        &tiff,
        ExportFormat::Tiff(options),
        tile_edge,
        ExportResize::Full,
    );
    assert_eq!(image::open(&tiff).unwrap().into_rgba8(), actual);

    for (original, actual) in case.source.pixels().iter().zip(actual.pixels()) {
        let working = working_pixel(case.adjustments, *original);
        let expected = encoded_srgb(working);
        for c in 0..3 {
            // Up to 1.5 steps of dither/rounding plus CPU/GPU half-float error.
            assert!((f32::from(actual[c]) - expected[c] * 255.0).abs() <= 2.0);
        }
        assert_eq!(actual[3], (working[3] * 255.0).round() as u8);
    }
    actual
}

/// GPU JPEG matches the CPU encoder on the same adjusted pixels; alpha is
/// flattened onto the chosen background.
fn check_jpeg_matches_cpu_encoder(case: &ExportCase, dir: &std::path::Path) {
    let (w, h) = (case.source.width(), case.source.height());
    let jpeg = dir.join("adjusted.jpg");
    case.run(&jpeg, ExportFormat::Jpeg(JPEG), 17, ExportResize::Full);
    let expected_path = dir.join("cpu.jpg");
    filter_io::write_srgb8_jpeg(
        std::fs::File::create(&expected_path).unwrap(),
        w,
        h,
        case.exif,
        JPEG,
        [Ok(case.adjusted_source().pixels().to_vec())],
    )
    .unwrap();
    let actual = filter_io::decode(&jpeg).unwrap();
    assert_eq!((actual.buffer.width(), actual.buffer.height()), (w, h));
    assert_eq!(
        actual.exif_bytes.as_deref(),
        Some(NORMALIZED_EXIF.as_slice())
    );
    assert!(actual.icc_profile.is_some());
    assert!(
        actual
            .buffer
            .pixels()
            .iter()
            .all(|p| p[3] == half::f16::ONE)
    );
    // Half-float CPU/GPU differences can cross RGB8 and DCT rounding
    // boundaries. Bound the decoded JPEG error in its 8-bit domain.
    assert_samples_close(
        image::open(&jpeg).unwrap().into_rgb8().as_raw(),
        image::open(&expected_path).unwrap().into_rgb8().as_raw(),
        4,
        "JPEG",
    );
}

/// Encode the CPU reference for `expected` pixels with the same format options.
fn write_reference(
    dir: &std::path::Path,
    format: ExportFormat,
    expected: &filter_core::ImageBuffer,
    exif: Option<&[u8]>,
) -> image::DynamicImage {
    let path = dir.join(format!("expected.{}", format.extension()));
    let output = std::fs::File::create(&path).unwrap();
    let (w, h) = (expected.width(), expected.height());
    let bands = [Ok(expected.pixels().to_vec())];
    match format {
        ExportFormat::Png(options) => filter_io::write_srgb_png(output, w, h, exif, options, bands),
        ExportFormat::Jpeg(options) => {
            filter_io::write_srgb8_jpeg(output, w, h, exif, options, bands)
        }
        ExportFormat::WebP(options) => {
            filter_io::write_srgb8_webp(output, w, h, exif, options, bands)
        }
        ExportFormat::Tiff(options) => filter_io::write_srgb_tiff(output, w, h, options, bands),
    }
    .unwrap();
    image::open(&path).unwrap()
}

/// Export one resized image and compare it with the CPU-resized reference.
/// Returns its RGBA16 pixels for seam and identity checks.
fn resized_export(
    case: &ExportCase,
    dir: &std::path::Path,
    format: ExportFormat,
    tile_edge: u32,
    resize: ExportResize,
    expected: &filter_core::ImageBuffer,
) -> image::ImageBuffer<image::Rgba<u16>, Vec<u16>> {
    let path = dir.join(format!("resized.{}", format.extension()));
    case.run(&path, format, tile_edge, resize);
    let actual = image::open(&path).unwrap();
    assert_eq!(
        (actual.width(), actual.height()),
        (expected.width(), expected.height())
    );
    let exif = (!matches!(format, ExportFormat::Tiff(_))).then_some(NORMALIZED_EXIF.as_slice());
    assert_export_metadata(&path, exif);
    let reference = write_reference(dir, format, expected, case.exif);
    let context = format!("{resize:?} {format:?}");
    let sixteen = matches!(
        format,
        ExportFormat::Png(PngOptions {
            bit_depth: PngBitDepth::Sixteen,
            ..
        }) | ExportFormat::Tiff(TiffOptions {
            bit_depth: TiffBitDepth::Sixteen,
            ..
        })
    );
    if sixteen {
        let tolerance = 128;
        let (a, e) = (actual.to_rgba16(), reference.into_rgba16());
        assert_samples_close(a.as_raw(), e.as_raw(), tolerance, &context);
    } else {
        let tolerance = if matches!(format, ExportFormat::Jpeg(_)) {
            4
        } else {
            2
        };
        let (a, e) = (actual.to_rgba8(), reference.into_rgba8());
        assert_samples_close(a.as_raw(), e.as_raw(), tolerance, &context);
    }
    actual.into_rgba16()
}

/// Resize happens after the captured nonlinear adjustments, across formats and
/// tile boundaries; identity sizes bypass filtering.
fn check_resized_exports_match_cpu(case: &ExportCase, dir: &std::path::Path) {
    let adjusted = case.adjusted_source();
    let formats = [
        ExportFormat::Png(PngOptions::default()),
        ExportFormat::Png(PngOptions {
            bit_depth: PngBitDepth::Eight,
            dither_seed: Some(42),
        }),
        ExportFormat::WebP(WebPOptions {
            dither_seed: Some(42),
        }),
        ExportFormat::Tiff(TiffOptions::default()),
        ExportFormat::Tiff(TiffOptions {
            bit_depth: TiffBitDepth::Eight,
            dither_seed: Some(42),
        }),
        ExportFormat::Jpeg(JPEG),
    ];
    let mut full_outputs = Vec::new();
    for resize in [
        ExportResize::Full,
        ExportResize::Percentage(100),
        ExportResize::LongEdge(1000),
        ExportResize::Percentage(50),
        ExportResize::LongEdge(20),
        ExportResize::Percentage(1),
    ] {
        let (ow, oh) = resize
            .dimensions(case.source.width(), case.source.height())
            .unwrap();
        let expected = adjusted.proxy(ow.max(oh));
        assert_eq!((expected.width(), expected.height()), (ow, oh));
        let identity = matches!(
            resize,
            ExportResize::Percentage(100) | ExportResize::LongEdge(1000)
        );
        for (index, format) in formats.into_iter().enumerate() {
            let mut previous = None;
            for edge in [7, 17, 31] {
                let pixels = resized_export(case, dir, format, edge, resize, &expected);
                if let Some(previous) = &previous {
                    assert_eq!(&pixels, previous, "tile seams changed resized output");
                }
                if edge == 7 && resize == ExportResize::Full {
                    full_outputs.push(pixels.clone());
                }
                if identity {
                    assert_eq!(
                        pixels, full_outputs[index],
                        "identity size must bypass filtering"
                    );
                }
                previous = Some(pixels);
            }
        }
    }
}

#[test]
#[ignore = "requires a software GPU"]
fn image_exports_render_the_captured_basic_adjustments() {
    let (device, queue) = software_device(wgpu::Limits::default());
    let (w, h) = (53, 11);
    let image = filter_core::ImageBuffer::new(
        w,
        h,
        (0..w * h)
            .map(|i| {
                let x = (i % w) as f32 / (w - 1) as f32;
                [
                    x * 0.4 + 0.02,
                    x * 0.2 + 0.1,
                    0.3,
                    [0.0, 0.25, 0.5, 1.0][i as usize % 4],
                ]
                .map(half::f16::from_f32)
            })
            .collect(),
    )
    .unwrap();
    let mut edit = filter_core::edit::EditState::default();
    edit.calibration.exposure = 0.4;
    edit.calibration.temperature = 0.3;
    edit.calibration.tint = -0.2;
    edit.tone.brightness = 0.5;
    edit.tone.contrast = 0.4;
    edit.tone.highlights = -0.3;
    edit.tone.shadows = 0.2;
    edit.tone.whites = -0.1;
    edit.tone.blacks = 0.15;
    edit.color.scalars.vibrance = 0.5;
    edit.color.scalars.saturation = 0.2;
    edit.curves.master.0 = vec![[0.0, 0.03], [0.4, 0.5], [1.0, 0.97]];
    edit.curves.green.0 = vec![[0.0, 0.0], [0.6, 0.5], [1.0, 1.0]];
    let snapshot = BasicAdjustments::new(&edit).unwrap();
    assert!(snapshot.curves().is_some());
    edit.calibration.exposure = -5.0;
    assert_ne!(
        snapshot.pixel([0.2; 4]),
        BasicAdjustments::new(&edit).unwrap().pixel([0.2; 4])
    );
    let directory = tempfile::tempdir().unwrap();
    let dir = directory.path();
    // The captured source metadata must travel through the tiled export too.
    let exif = EXIF;
    let case = ExportCase {
        device: &device,
        queue: &queue,
        source: &image,
        exif: Some(&exif),
        adjustments: &snapshot,
    };
    let png16 = dir.join("adjusted.png");
    case.run(
        &png16,
        ExportFormat::Png(PngOptions::default()),
        17,
        ExportResize::Full,
    );
    assert_png16_matches_cpu(&case, &png16);
    assert_eq!(exif[18], 6, "export must not mutate the source metadata");
    check_tiff16_matches_png16(&case, dir, &png16);
    check_8bit_formats_agree_across_tiles(&case, dir);
    check_jpeg_matches_cpu_encoder(&case, dir);
    check_resized_exports_match_cpu(&case, dir);
}

/// Checkerboard of four gray levels in 73 × 71 pixel cells, with alpha
/// cycling diagonally so every tile sees every alpha.
const LEVELS: [f32; 4] = [0.0625, 0.125, 0.25, 0.5];
const ALPHAS: [f32; 4] = [0.0, 0.25, 0.5, 1.0];
fn level_at(x: u32, y: u32) -> usize {
    ((x / 73 + y / 71) % 4) as usize
}
fn alpha_at(x: u32, y: u32) -> usize {
    ((x + y) % 4) as usize
}

fn report(what: &str, case: &ExportCase, started: Instant) {
    let pixels = u64::from(case.source.width()) * u64::from(case.source.height());
    let tiles = TilePlan::new(
        case.source.width(),
        case.source.height(),
        &case.device.limits(),
        1024,
    )
    .unwrap()
    .tile_count();
    eprintln!(
        "Validated {pixels} {what} across {tiles} tiles in {:.2?}",
        started.elapsed()
    );
}

/// Exposure +1 doubles each level; 16-bit output is checked row by row.
fn check_large_png16(case: &ExportCase, path: &std::path::Path) {
    let (w, h) = (case.source.width(), case.source.height());
    let mut reader = png_reader(path);
    assert_eq!((reader.info().width, reader.info().height), (w, h));
    assert_eq!(
        reader.output_color_type(),
        (png::ColorType::Rgba, png::BitDepth::Sixteen)
    );
    assert!(reader.info().icc_profile.is_some());
    assert!(reader.info().exif_metadata.is_none());
    let expected_rgb = LEVELS.map(|v| {
        filter_core::color::rec2020_to_linear_srgb([v * 2.0; 3]).map(|c| {
            (filter_core::color::linear_to_srgb(c).clamp(0.0, 1.0) * 65535.0).round() as i32
        })
    });
    let expected_alpha = ALPHAS.map(|v| (v * 65535.0).round() as u16);
    for y in 0..h {
        let row = reader.next_row().unwrap().expect("missing output row");
        assert_eq!(row.data().len(), w as usize * 8);
        for (x, pixel) in (0..).zip(row.data().as_chunks::<8>().0) {
            let actual: [u16; 4] =
                std::array::from_fn(|c| u16::from_be_bytes([pixel[c * 2], pixel[c * 2 + 1]]));
            let expected = expected_rgb[level_at(x, y)];
            for c in 0..3 {
                assert!(
                    (i32::from(actual[c]) - expected[c]).abs() <= 1,
                    "pixel {x},{y}: {actual:?}"
                );
            }
            assert_eq!(actual[3], expected_alpha[alpha_at(x, y)]);
        }
    }
    assert!(reader.next_row().unwrap().is_none());
    reader.finish().unwrap();
}

fn check_large_png8(case: &ExportCase, path: &std::path::Path) {
    let (w, h) = (case.source.width(), case.source.height());
    let mut reader = png_reader(path);
    assert_eq!((reader.info().width, reader.info().height), (w, h));
    assert_eq!(
        reader.output_color_type(),
        (png::ColorType::Rgba, png::BitDepth::Eight)
    );
    assert!(reader.info().icc_profile.is_some());
    let expected_rgb = LEVELS.map(|v| {
        filter_core::color::rec2020_to_linear_srgb([v * 2.0; 3])
            .map(|c| filter_core::color::linear_to_srgb(c).clamp(0.0, 1.0) * 255.0)
    });
    let expected_alpha = ALPHAS.map(|v| (v * 255.0).round() as u8);
    for y in 0..h {
        let row = reader.next_row().unwrap().expect("missing PNG8 row");
        assert_eq!(row.data().len(), w as usize * 4);
        for (x, actual) in (0..).zip(row.data().as_chunks::<4>().0) {
            let expected = expected_rgb[level_at(x, y)];
            for c in 0..3 {
                assert!(
                    (f32::from(actual[c]) - expected[c]).abs() <= 1.501,
                    "PNG8 pixel {x},{y}: {actual:?} vs {expected:?}"
                );
            }
            assert_eq!(actual[3], expected_alpha[alpha_at(x, y)]);
        }
    }
    assert!(reader.next_row().unwrap().is_none());
    reader.finish().unwrap();
}

/// Lossless WebP reproduces PNG8 exactly, compared one row at a time.
fn check_large_webp_matches_png8(
    case: &ExportCase,
    webp: &std::path::Path,
    png8: &std::path::Path,
) {
    use image::ImageDecoder;
    let (w, h) = (case.source.width(), case.source.height());
    let mut decoder = image::codecs::webp::WebPDecoder::new(std::io::BufReader::new(
        std::fs::File::open(webp).unwrap(),
    ))
    .unwrap();
    assert_eq!(decoder.dimensions(), (w, h));
    assert_eq!(decoder.color_type(), image::ColorType::Rgba8);
    assert!(decoder.icc_profile().unwrap().is_some());
    assert!(decoder.exif_metadata().unwrap().is_none());
    let rgba = image::DynamicImage::from_decoder(decoder)
        .unwrap()
        .into_rgba8();
    let mut png = png_reader(png8);
    for row in rgba.as_raw().chunks_exact(w as usize * 4) {
        assert_eq!(row, png.next_row().unwrap().unwrap().data());
    }
    assert!(png.next_row().unwrap().is_none());
    png.finish().unwrap();
}

/// TIFF strips match the PNG of the same bit depth, one strip and row at a
/// time; never a full output image.
fn check_large_tiff_matches_png(
    case: &ExportCase,
    tiff: &std::path::Path,
    bit_depth: TiffBitDepth,
    reference: &std::path::Path,
) {
    let (w, h) = (case.source.width(), case.source.height());
    let mut decoder =
        tiff::decoder::Decoder::new(std::io::BufReader::new(std::fs::File::open(tiff).unwrap()))
            .unwrap();
    assert_eq!(decoder.dimensions().unwrap(), (w, h));
    assert_eq!(
        decoder
            .get_tag_u16_vec(tiff::tags::Tag::ExtraSamples)
            .unwrap(),
        [2]
    );
    assert_eq!(
        decoder.get_tag_u32(tiff::tags::Tag::Orientation).unwrap(),
        1
    );
    assert!(
        !decoder
            .get_tag_u8_vec(tiff::tags::Tag::IccProfile)
            .unwrap()
            .is_empty()
    );
    let mut reader = png_reader(reference);
    let mut rows = 0;
    let row_samples = w as usize * 4;
    for strip in 0..decoder.strip_count().unwrap() {
        match decoder.read_chunk(strip).unwrap() {
            tiff::decoder::DecodingResult::U8(samples) => {
                assert_eq!(bit_depth, TiffBitDepth::Eight);
                for row in samples.chunks_exact(row_samples) {
                    assert_eq!(row, reader.next_row().unwrap().unwrap().data());
                    rows += 1;
                }
            }
            tiff::decoder::DecodingResult::U16(samples) => {
                assert_eq!(bit_depth, TiffBitDepth::Sixteen);
                for row in samples.chunks_exact(row_samples) {
                    let expected = reader.next_row().unwrap().unwrap();
                    let expected: Vec<u16> = expected
                        .data()
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|b| u16::from_be_bytes(*b))
                        .collect();
                    assert_eq!(row, expected.as_slice());
                    rows += 1;
                }
            }
            other => panic!("unexpected TIFF samples: {other:?}"),
        }
    }
    assert_eq!(rows, h);
    assert!(reader.next_row().unwrap().is_none());
    reader.finish().unwrap();
}

/// JPEG flattens alpha onto white before encoding.
fn check_large_jpeg(case: &ExportCase, path: &std::path::Path) {
    use image::ImageDecoder;
    let w = case.source.width();
    let mut decoder = image::codecs::jpeg::JpegDecoder::new(std::io::BufReader::new(
        std::fs::File::open(path).unwrap(),
    ))
    .unwrap();
    assert_eq!(decoder.dimensions(), (w, case.source.height()));
    assert_eq!(decoder.color_type(), image::ColorType::Rgb8);
    assert!(decoder.icc_profile().unwrap().is_some());
    assert!(decoder.exif_metadata().unwrap().is_none());
    let mut rgb = vec![0; decoder.total_bytes() as usize];
    decoder.read_image(&mut rgb).unwrap();
    for (i, actual) in (0..).zip(rgb.as_chunks::<3>().0) {
        let (x, y) = (i % w, i / w);
        let level = LEVELS[level_at(x, y)] * 2.0;
        let alpha = ALPHAS[alpha_at(x, y)];
        let expected = (filter_core::color::linear_to_srgb(level * alpha + 1.0 - alpha) * 255.0)
            .round() as i32;
        for channel in actual {
            assert!(
                (i32::from(*channel) - expected).abs() <= 4,
                "JPEG pixel {x},{y}: {actual:?} vs {expected}"
            );
        }
    }
}

/// Heavy reduction still renders every source tile with bounded row state.
fn check_large_reduction(case: &ExportCase, path: &std::path::Path) {
    let expected = case.source.proxy(257);
    let adjusted: Vec<_> = expected
        .pixels()
        .iter()
        .map(|p| {
            [
                half::f16::from_f32(p[0].to_f32() * 2.0),
                half::f16::from_f32(p[1].to_f32() * 2.0),
                half::f16::from_f32(p[2].to_f32() * 2.0),
                p[3],
            ]
        })
        .collect();
    let mut encoded = Vec::new();
    filter_io::write_srgb16_png(
        &mut encoded,
        expected.width(),
        expected.height(),
        [Ok(adjusted)],
    )
    .unwrap();
    assert_eq!(
        image::open(path).unwrap().into_rgba16(),
        image::load_from_memory(&encoded).unwrap().into_rgba16()
    );
}

#[test]
#[ignore = "requires a software GPU and about 800 MiB RAM; exercises >36MP image exports"]
fn large_image_exports_exceed_texture_and_full_readback_limits() {
    let (device, queue) = software_device(wgpu::Limits {
        max_texture_dimension_2d: 2048,
        ..Default::default()
    });
    let (w, h) = (6101_u32, 6001_u32);
    assert!(w > device.limits().max_texture_dimension_2d);
    assert!(u64::from(w) * u64::from(h) * 8 > device.limits().max_buffer_size);
    let source = filter_core::ImageBuffer::new(
        w,
        h,
        (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let level = LEVELS[level_at(x, y)];
                [level, level, level, ALPHAS[alpha_at(x, y)]].map(half::f16::from_f32)
            })
            .collect(),
    )
    .unwrap();
    let adjustments = BasicAdjustments::exposure(1.0).unwrap();
    let case = ExportCase {
        device: &device,
        queue: &queue,
        source: &source,
        exif: None,
        adjustments: &adjustments,
    };
    let directory = tempfile::tempdir().unwrap();
    let dir = directory.path();
    let started = Instant::now();

    let png16 = dir.join("large.png");
    std::fs::write(&png16, b"old destination").unwrap();
    case.run(
        &png16,
        ExportFormat::Png(PngOptions::default()),
        1024,
        ExportResize::Full,
    );
    // Verify every decoded output pixel without allocating another full image.
    check_large_png16(&case, &png16);

    let png8_started = Instant::now();
    let png8 = dir.join("large8.png");
    let options = PngOptions {
        bit_depth: PngBitDepth::Eight,
        ..Default::default()
    };
    case.run(&png8, ExportFormat::Png(options), 1024, ExportResize::Full);
    check_large_png8(&case, &png8);
    report("PNG8 pixels", &case, png8_started);

    let webp_started = Instant::now();
    let webp = dir.join("large.webp");
    case.run(
        &webp,
        ExportFormat::WebP(WebPOptions::default()),
        1024,
        ExportResize::Full,
    );
    check_large_webp_matches_png8(&case, &webp, &png8);
    report("lossless WebP pixels", &case, webp_started);

    for (bit_depth, reference) in [
        (TiffBitDepth::Eight, &png8),
        (TiffBitDepth::Sixteen, &png16),
    ] {
        let tiff_started = Instant::now();
        let tiff = dir.join("large.tiff");
        std::fs::write(&tiff, b"old destination").unwrap();
        let options = TiffOptions {
            bit_depth,
            ..Default::default()
        };
        case.run(&tiff, ExportFormat::Tiff(options), 1024, ExportResize::Full);
        check_large_tiff_matches_png(&case, &tiff, bit_depth, reference);
        report(
            &format!("{bit_depth:?}-bit TIFF pixels"),
            &case,
            tiff_started,
        );
    }

    let jpeg_started = Instant::now();
    let jpeg = dir.join("large.jpg");
    let options = JpegOptions {
        quality: 100,
        dither_seed: None,
        ..Default::default()
    };
    case.run(&jpeg, ExportFormat::Jpeg(options), 1024, ExportResize::Full);
    check_large_jpeg(&case, &jpeg);
    report("JPEG pixels", &case, jpeg_started);

    let resize_started = Instant::now();
    let resized = dir.join("large-resized.png");
    case.run(
        &resized,
        ExportFormat::Png(PngOptions::default()),
        1024,
        ExportResize::LongEdge(257),
    );
    check_large_reduction(&case, &resized);
    eprintln!(
        "Validated 36.6MP reduction to 257 px across its tiles in {:.2?}",
        resize_started.elapsed()
    );
    report("pixels", &case, started);
}

/// A film-like lattice (slope below 3, cross-channel terms) of N^3 nodes.
fn film_lut(n: usize) -> std::sync::Arc<filter_core::lut::CubeLut> {
    let mut texels = Vec::with_capacity(3 * n * n * n);
    let s = (n - 1) as f32;
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let [x, y, z] = [r, g, b].map(|v| v as f32 / s);
                let out = [
                    0.9 * x.powf(1.1) + 0.04 * y + 0.037,
                    0.05 + 0.85 * y + 0.1 * (z - x),
                    0.1 + 0.7 * z + 0.15 * x * y,
                ];
                texels.extend(out.map(|v| half::f16::from_f32(v).to_bits()));
            }
        }
    }
    std::sync::Arc::new(
        filter_core::lut::CubeLut::new(n, "film", [0.0; 3], [1.0; 3], texels).unwrap(),
    )
}
fn lut_snapshot(
    lut: &std::sync::Arc<filter_core::lut::CubeLut>,
    exposure: f32,
) -> BasicAdjustments {
    let mut edit = filter_core::edit::EditState::default();
    edit.calibration.exposure = exposure;
    edit.finishing.lut = filter_core::lut::LutSlot::Cube(lut.clone());
    BasicAdjustments::new(&edit).unwrap()
}
fn gradient_source(w: u32, h: u32) -> filter_core::ImageBuffer {
    filter_core::ImageBuffer::new(
        w,
        h,
        (0..w * h)
            .map(|i| {
                let x = (i % w) as f32 / (w - 1) as f32;
                let y = (i / w) as f32 / (h - 1) as f32;
                [
                    0.3 + x * 0.4,
                    0.35 + y * 0.25,
                    0.5 - x * 0.2 + y * 0.1,
                    ALPHAS[i as usize % 4],
                ]
                .map(half::f16::from_f32)
            })
            .collect(),
    )
    .unwrap()
}

#[test]
#[ignore = "requires a software GPU"]
fn image_exports_render_the_captured_lut_in_every_format_and_resize() {
    let (device, queue) = software_device(wgpu::Limits::default());
    let image = gradient_source(53, 11);
    let snapshot = lut_snapshot(&film_lut(17), 0.3);
    let exposure_only = BasicAdjustments::exposure(0.3).unwrap();
    assert!(
        image
            .pixels()
            .iter()
            .any(|p| snapshot.pixel(p.map(half::f16::to_f32))[..3]
                != exposure_only.pixel(p.map(half::f16::to_f32))[..3]),
        "the LUT must change the export"
    );
    let directory = tempfile::tempdir().unwrap();
    let dir = directory.path();
    let exif = EXIF;
    let case = ExportCase {
        device: &device,
        queue: &queue,
        source: &image,
        exif: Some(&exif),
        adjustments: &snapshot,
    };
    let png16 = dir.join("lut.png");
    case.run(
        &png16,
        ExportFormat::Png(PngOptions::default()),
        17,
        ExportResize::Full,
    );
    assert_png16_matches_cpu(&case, &png16);
    check_tiff16_matches_png16(&case, dir, &png16);
    check_8bit_formats_agree_across_tiles(&case, dir);
    check_jpeg_matches_cpu_encoder(&case, dir);
    check_resized_exports_match_cpu(&case, dir);
}

#[test]
#[ignore = "requires a software GPU"]
fn export_snapshot_keeps_the_lut_captured_before_a_later_edit() {
    let (device, queue) = software_device(wgpu::Limits::default());
    let image = gradient_source(40, 9);
    let first = film_lut(17);
    let snapshot = lut_snapshot(&first, 0.0);
    // A later edit swaps the LUT; the captured snapshot is unaffected.
    let later = lut_snapshot(&film_lut(33), 0.0);
    assert_ne!(
        snapshot.pixel([0.4, 0.3, 0.2, 1.0]),
        later.pixel([0.4, 0.3, 0.2, 1.0])
    );
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("snapshot.png");
    let case = ExportCase {
        device: &device,
        queue: &queue,
        source: &image,
        exif: None,
        adjustments: &snapshot,
    };
    case.run(
        &path,
        ExportFormat::Png(PngOptions::default()),
        13,
        ExportResize::Full,
    );
    let mut reader = png_reader(&path);
    for row in image.pixels().as_chunks::<40>().0 {
        let actual = reader.next_row().unwrap().unwrap();
        for (original, bytes) in row.iter().zip(actual.data().as_chunks::<8>().0) {
            let expected = encoded_srgb(working_pixel(&snapshot, *original))
                .map(|c| (c * 65535.0).round() as i32);
            for c in 0..3 {
                let got = i32::from(u16::from_be_bytes([bytes[c * 2], bytes[c * 2 + 1]]));
                assert!((got - expected[c]).abs() <= 128, "{got} vs {}", expected[c]);
            }
        }
    }
}

#[test]
#[ignore = "requires a software GPU and about 800 MiB RAM; exercises >36MP image exports"]
fn large_image_exports_apply_a_65_cubed_lut_across_tiles() {
    let (device, queue) = software_device(wgpu::Limits {
        max_texture_dimension_2d: 2048,
        ..Default::default()
    });
    let (w, h) = (6101_u32, 6001_u32);
    let source = filter_core::ImageBuffer::new(
        w,
        h,
        (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let level = LEVELS[level_at(x, y)];
                [level, level, level, ALPHAS[alpha_at(x, y)]].map(half::f16::from_f32)
            })
            .collect(),
    )
    .unwrap();
    let adjustments = lut_snapshot(&film_lut(65), 1.0);
    let case = ExportCase {
        device: &device,
        queue: &queue,
        source: &source,
        exif: None,
        adjustments: &adjustments,
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("large-lut.png");
    let started = Instant::now();
    case.run(
        &path,
        ExportFormat::Png(PngOptions::default()),
        1024,
        ExportResize::Full,
    );
    let expected = LEVELS.map(|v| {
        let gray = half::f16::from_f32(v);
        let pixel = working_pixel(&adjustments, [gray, gray, gray, half::f16::ONE]);
        encoded_srgb(pixel).map(|c| (c * 65535.0).round() as i32)
    });
    let mut reader = png_reader(&path);
    for y in 0..h {
        let row = reader.next_row().unwrap().expect("missing output row");
        for (x, bytes) in (0..).zip(row.data().as_chunks::<8>().0) {
            for c in 0..3 {
                let got = i32::from(u16::from_be_bytes([bytes[c * 2], bytes[c * 2 + 1]]));
                let want = expected[level_at(x, y)][c];
                assert!((got - want).abs() <= 128, "pixel {x},{y}: {got} vs {want}");
            }
        }
    }
    assert!(reader.next_row().unwrap().is_none());
    report("LUT pixels", &case, started);
}
