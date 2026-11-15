use anyhow::{Context, Result, ensure};
use filter_core::cpu_eval::BasicAdjustments;
use filter_gpu::{TilePlan, TiledExposure};
use filter_io::{DecodedImage, JpegOptions};
use std::io::Write;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug)]
pub enum ExportFormat {
    Png,
    Jpeg(JpegOptions),
}
impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg(_) => "jpg",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Png => "16-bit PNG",
            Self::Jpeg(_) => "JPEG",
        }
    }
    fn validate_path(self, path: &std::path::Path) -> Result<()> {
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default();
        let valid = match self {
            Self::Png => extension.eq_ignore_ascii_case("png"),
            Self::Jpeg(_) => {
                extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg")
            }
        };
        ensure!(
            valid,
            "{} export requires a filename ending in {}",
            self.label(),
            match self {
                Self::Png => ".png",
                Self::Jpeg(_) => ".jpg or .jpeg",
            }
        );
        Ok(())
    }
}

pub enum ExportResult {
    Saved(PathBuf),
    Cancelled,
    Failed(String),
}

pub struct ExportJob {
    total: u64,
    completed: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    result: mpsc::Receiver<ExportResult>,
}
impl ExportJob {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        device: wgpu::Device,
        queue: wgpu::Queue,
        source: Arc<DecodedImage>,
        adjustments: BasicAdjustments,
        path: PathBuf,
        format: ExportFormat,
        ctx: eframe::egui::Context,
    ) -> Result<Self> {
        format.validate_path(&path)?;
        let plan = TilePlan::new(
            source.buffer.width(),
            source.buffer.height(),
            &device.limits(),
            1024,
        )?;
        let total = plan.tile_count() + 1;
        let completed = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, result) = mpsc::channel();
        let job = Self {
            total,
            completed: completed.clone(),
            cancel: cancel.clone(),
            result,
        };
        std::thread::Builder::new()
            .name("image-export".into())
            .spawn(move || {
                let result = export_image(
                    &device,
                    &queue,
                    &source.buffer,
                    source.exif_bytes.as_deref(),
                    &adjustments,
                    &path,
                    format,
                    plan,
                    &cancel,
                    &completed,
                );
                let message = match result {
                    Ok(()) => {
                        completed.fetch_add(1, Ordering::Relaxed);
                        ExportResult::Saved(path)
                    }
                    Err(_) if cancel.load(Ordering::Relaxed) => ExportResult::Cancelled,
                    Err(error) => ExportResult::Failed(format!("{error:#}")),
                };
                let _ = tx.send(message);
                ctx.request_repaint();
            })
            .context("Could not start export worker")?;
        Ok(job)
    }
    pub fn progress(&self) -> f32 {
        self.completed.load(Ordering::Relaxed) as f32 / self.total as f32
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelling(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    pub fn try_result(&self) -> Option<ExportResult> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(ExportResult::Failed(
                "Export worker stopped unexpectedly".into(),
            )),
        }
    }
}
impl Drop for ExportJob {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[allow(clippy::too_many_arguments)]
fn export_image(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    source: &filter_core::ImageBuffer,
    exif: Option<&[u8]>,
    adjustments: &BasicAdjustments,
    path: &std::path::Path,
    format: ExportFormat,
    plan: TilePlan,
    cancel: &AtomicBool,
    completed: &AtomicU64,
) -> Result<()> {
    let renderer = TiledExposure::new(device);
    let mut tiles = plan.tiles().peekable();
    let bands = std::iter::from_fn(|| {
        let first = *tiles.peek()?;
        Some((|| -> Result<_> {
            ensure!(!cancel.load(Ordering::Relaxed), "Export cancelled");
            let width = source.width() as usize;
            let count = width
                .checked_mul(first.height as usize)
                .context("Export band dimensions overflow")?;
            let mut band = Vec::new();
            band.try_reserve_exact(count)
                .context("Could not allocate export row band")?;
            band.resize(count, Default::default());
            while tiles.peek().is_some_and(|tile| tile.y == first.y) {
                ensure!(!cancel.load(Ordering::Relaxed), "Export cancelled");
                let tile = tiles.next().expect("peeked tile");
                let mut pending =
                    renderer.submit_adjusted(device, queue, source, tile, adjustments)?;
                let started = Instant::now();
                let pixels = loop {
                    ensure!(!cancel.load(Ordering::Relaxed), "Export cancelled");
                    device
                        .poll(wgpu::PollType::Poll)
                        .context("GPU device poll failed")?;
                    if let Some(pixels) = pending.try_read()? {
                        break pixels;
                    }
                    ensure!(
                        started.elapsed() < Duration::from_secs(60),
                        "GPU tile readback timed out"
                    );
                    std::thread::sleep(Duration::from_millis(2));
                };
                for (row, pixels) in pixels.chunks_exact(tile.width as usize).enumerate() {
                    let start = row * width + tile.x as usize;
                    band[start..start + pixels.len()].copy_from_slice(pixels);
                }
                completed.fetch_add(1, Ordering::Relaxed);
                // One tile per submission and at most one in flight; let UI work enter the queue.
                std::thread::yield_now();
            }
            Ok(band)
        })())
    });
    publish_export(path, cancel, |file| {
        // Buffer JPEG's small writes, checking cancellation throughout encoding too.
        let mut output = std::io::BufWriter::with_capacity(
            64 * 1024,
            CancelWriter {
                output: file,
                cancel,
            },
        );
        match format {
            ExportFormat::Png => filter_io::write_srgb16_png_with_exif(
                &mut output,
                source.width(),
                source.height(),
                exif,
                bands,
            )?,
            ExportFormat::Jpeg(options) => filter_io::write_srgb8_jpeg_cancellable(
                &mut output,
                source.width(),
                source.height(),
                exif,
                options,
                bands,
                || cancel.load(Ordering::Relaxed),
            )?,
        }
        output.flush()?;
        Ok(())
    })
}

fn publish_export(
    path: &std::path::Path,
    cancel: &AtomicBool,
    write: impl FnOnce(&mut std::fs::File) -> Result<()>,
) -> Result<()> {
    // A cancelled or failed render never replaces an existing destination.
    ensure!(!cancel.load(Ordering::Relaxed), "Export cancelled");
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let mut builder = tempfile::Builder::new();
    builder.prefix(".photo-filter-");
    #[cfg(unix)]
    let existing_mode = {
        use std::os::unix::fs::PermissionsExt;
        let existing_mode = match std::fs::metadata(path) {
            Ok(metadata) => Some(metadata.permissions().mode() & 0o777),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error).context("Could not inspect export destination"),
        };
        // Keep replacements private while writing; restore their original bits
        // only once complete. New files use normal creation/umask defaults.
        builder.permissions(std::fs::Permissions::from_mode(
            if existing_mode.is_some() {
                0o600
            } else {
                0o666
            },
        ));
        existing_mode
    };
    let mut temporary = builder
        .tempfile_in(parent)
        .context("Could not create export temporary file")?;
    write(temporary.as_file_mut())?;
    #[cfg(unix)]
    if let Some(mode) = existing_mode {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(mode))
            .context("Could not preserve destination permissions")?;
    }
    temporary
        .as_file()
        .sync_all()
        .context("Could not flush export file")?;
    ensure!(!cancel.load(Ordering::Relaxed), "Export cancelled");
    temporary
        .persist(path)
        .map_err(|e| e.error)
        .context("Could not replace export destination")?;
    Ok(())
}

struct CancelWriter<'a, W> {
    output: W,
    cancel: &'a AtomicBool,
}
impl<W: Write> Write for CancelWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("Export cancelled"));
        }
        self.output.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("Export cancelled"));
        }
        self.output.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn export_requires_an_extension_matching_the_selected_format() {
        assert!(
            ExportFormat::Png
                .validate_path(std::path::Path::new("photo.png"))
                .is_ok()
        );
        assert!(
            ExportFormat::Png
                .validate_path(std::path::Path::new("photo.PNG"))
                .is_ok()
        );
        assert!(
            ExportFormat::Png
                .validate_path(std::path::Path::new("photo.jpg"))
                .is_err()
        );
        assert!(
            ExportFormat::Png
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
    fn jpeg_cancellation_during_encoding_preserves_destination() {
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
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("edited.jpg");
        std::fs::write(&path, b"original").unwrap();
        let cancel = AtomicBool::new(false);
        let result = publish_export(&path, &cancel, |file| {
            filter_io::write_srgb8_jpeg(
                CancelWriter {
                    output: CancelAfterWrite {
                        file,
                        cancel: &cancel,
                    },
                    cancel: &cancel,
                },
                17,
                9,
                None,
                JpegOptions::default(),
                [Ok(vec![[half::f16::ONE; 4]; 17 * 9])],
            )
        });
        assert!(result.is_err());
        assert!(cancel.load(Ordering::Relaxed));
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
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
    #[ignore = "requires a software GPU"]
    fn png_and_jpeg_export_render_the_captured_basic_adjustments() {
        pollster::block_on(async {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::PRIMARY,
                ..wgpu::InstanceDescriptor::new_without_display_handle()
            });
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    force_fallback_adapter: true,
                    ..Default::default()
                })
                .await
                .expect("software adapter required");
            let (device, queue) = adapter.request_device(&Default::default()).await.unwrap();
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
            let snapshot = BasicAdjustments::new(&edit).unwrap();
            edit.calibration.exposure = -5.0;
            assert_ne!(
                snapshot.pixel([0.2; 4]),
                BasicAdjustments::new(&edit).unwrap().pixel([0.2; 4])
            );
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("adjusted.png");
            let plan = TilePlan::new(w, h, &device.limits(), 17).unwrap();
            let count = plan.tile_count();
            let completed = AtomicU64::new(0);
            // The captured source metadata must travel through the tiled export too.
            let exif = [
                73, 73, 42, 0, 8, 0, 0, 0, 1, 0, 18, 1, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
            ];
            export_image(
                &device,
                &queue,
                &image,
                Some(&exif),
                &snapshot,
                &path,
                ExportFormat::Png,
                plan,
                &AtomicBool::new(false),
                &completed,
            )
            .unwrap();
            assert_eq!(completed.load(Ordering::Relaxed), count);
            let mut reader =
                png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).unwrap()))
                    .read_info()
                    .unwrap();
            assert!(reader.info().icc_profile.is_some());
            let mut expected_exif = exif;
            expected_exif[18] = 1;
            assert_eq!(
                reader.info().exif_metadata.as_deref(),
                Some(expected_exif.as_slice())
            );
            assert_eq!(exif[18], 6, "export must not mutate the source metadata");
            assert_eq!(
                reader.output_color_type(),
                (png::ColorType::Rgba, png::BitDepth::Sixteen)
            );
            for row in image.pixels().chunks_exact(w as usize) {
                let actual = reader.next_row().unwrap().unwrap();
                for (original, bytes) in row.iter().zip(actual.data().as_chunks::<8>().0) {
                    // Match the renderer's half-float storage boundary before encoding.
                    let working = snapshot
                        .pixel(original.map(half::f16::to_f32))
                        .map(|v| half::f16::from_f32(v).to_f32());
                    let rgb = filter_core::color::rec2020_to_linear_srgb(
                        working[..3].try_into().unwrap(),
                    );
                    let expected = rgb.map(|c| {
                        (filter_core::color::linear_to_srgb(c).clamp(0.0, 1.0) * 65535.0).round()
                            as i32
                    });
                    for c in 0..3 {
                        let actual =
                            i32::from(u16::from_be_bytes([bytes[c * 2], bytes[c * 2 + 1]]));
                        assert!(
                            // Half-float rounding can differ by one ULP across CPU/GPU.
                            // Bound final sRGB error to less than half an 8-bit step.
                            (actual - expected[c]).abs() <= 128,
                            "{actual} != {}",
                            expected[c]
                        );
                    }
                    assert_eq!(
                        u16::from_be_bytes([bytes[6], bytes[7]]),
                        (working[3] * 65535.0).round() as u16
                    );
                }
            }
            assert!(reader.next_row().unwrap().is_none());
            reader.finish().unwrap();
            let jpeg_path = directory.path().join("adjusted.jpg");
            let options = JpegOptions {
                quality: 100,
                background: [81, 142, 203],
                dither_seed: None,
            };
            completed.store(0, Ordering::Relaxed);
            export_image(
                &device,
                &queue,
                &image,
                Some(&exif),
                &snapshot,
                &jpeg_path,
                ExportFormat::Jpeg(options),
                TilePlan::new(w, h, &device.limits(), 17).unwrap(),
                &AtomicBool::new(false),
                &completed,
            )
            .unwrap();
            assert_eq!(completed.load(Ordering::Relaxed), count);
            let expected_path = directory.path().join("cpu.jpg");
            let pixels = image
                .pixels()
                .iter()
                .map(|pixel| {
                    snapshot
                        .pixel(pixel.map(half::f16::to_f32))
                        .map(half::f16::from_f32)
                })
                .collect();
            filter_io::write_srgb8_jpeg(
                std::fs::File::create(&expected_path).unwrap(),
                w,
                h,
                Some(&exif),
                options,
                [Ok(pixels)],
            )
            .unwrap();
            let actual = filter_io::decode(&jpeg_path).unwrap();
            assert_eq!((actual.buffer.width(), actual.buffer.height()), (w, h));
            assert_eq!(actual.exif_bytes.as_deref(), Some(expected_exif.as_slice()));
            assert!(actual.icc_profile.is_some());
            assert!(
                actual
                    .buffer
                    .pixels()
                    .iter()
                    .all(|p| p[3] == half::f16::ONE)
            );
            let actual_rgb = image::open(&jpeg_path).unwrap().into_rgb8();
            let expected_rgb = image::open(&expected_path).unwrap().into_rgb8();
            for (actual, expected) in actual_rgb.pixels().zip(expected_rgb.pixels()) {
                for c in 0..3 {
                    // Half-float CPU/GPU differences can cross RGB8 and DCT rounding
                    // boundaries. Bound the decoded JPEG error in its 8-bit domain.
                    assert!(
                        (i16::from(actual[c]) - i16::from(expected[c])).abs() <= 4,
                        "{actual:?} vs {expected:?}"
                    );
                }
            }
        });
    }

    #[test]
    #[ignore = "requires a software GPU and about 550 MiB RAM; exercises >36MP PNG/JPEG export"]
    fn large_png_and_jpeg_export_exceeds_texture_and_full_readback_limits() {
        pollster::block_on(async {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::PRIMARY,
                ..wgpu::InstanceDescriptor::new_without_display_handle()
            });
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    force_fallback_adapter: true,
                    ..Default::default()
                })
                .await
                .expect("software adapter required");
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    required_limits: wgpu::Limits {
                        max_texture_dimension_2d: 2048,
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .await
                .unwrap();
            let (w, h) = (6101_u32, 6001_u32);
            assert!(w > device.limits().max_texture_dimension_2d);
            assert!(u64::from(w) * u64::from(h) * 8 > device.limits().max_buffer_size);
            let levels = [0.0625_f32, 0.125, 0.25, 0.5];
            let alphas = [0.0_f32, 0.25, 0.5, 1.0];
            let source = filter_core::ImageBuffer::new(
                w,
                h,
                (0..w * h)
                    .map(|i| {
                        let x = i % w;
                        let y = i / w;
                        let level = levels[((x / 73 + y / 71) % 4) as usize];
                        [level, level, level, alphas[((x + y) % 4) as usize]]
                            .map(half::f16::from_f32)
                    })
                    .collect(),
            )
            .unwrap();
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("large.png");
            std::fs::write(&path, b"old destination").unwrap();
            let cancel = AtomicBool::new(false);
            let completed = AtomicU64::new(0);
            let plan = TilePlan::new(w, h, &device.limits(), 1024).unwrap();
            let total = plan.tile_count();
            let started = Instant::now();
            export_image(
                &device,
                &queue,
                &source,
                None,
                &BasicAdjustments::exposure(1.0).unwrap(),
                &path,
                ExportFormat::Png,
                plan,
                &cancel,
                &completed,
            )
            .unwrap();
            assert_eq!(completed.load(Ordering::Relaxed), total);
            // Verify every decoded output pixel without allocating another full image.
            let mut reader =
                png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()))
                    .read_info()
                    .unwrap();
            assert_eq!((reader.info().width, reader.info().height), (w, h));
            assert_eq!(
                reader.output_color_type(),
                (png::ColorType::Rgba, png::BitDepth::Sixteen)
            );
            assert!(reader.info().icc_profile.is_some());
            assert!(reader.info().exif_metadata.is_none());
            let expected_rgb = levels.map(|v| {
                filter_core::color::rec2020_to_linear_srgb([v * 2.0; 3]).map(|c| {
                    (filter_core::color::linear_to_srgb(c).clamp(0.0, 1.0) * 65535.0).round() as i32
                })
            });
            let expected_alpha = alphas.map(|v| (v * 65535.0).round() as u16);
            for y in 0..h {
                let row = reader.next_row().unwrap().expect("missing output row");
                assert_eq!(row.data().len(), w as usize * 8);
                for (x, pixel) in row.data().as_chunks::<8>().0.iter().enumerate() {
                    let actual: [u16; 4] = std::array::from_fn(|c| {
                        u16::from_be_bytes([pixel[c * 2], pixel[c * 2 + 1]])
                    });
                    let expected = expected_rgb[((x as u32 / 73 + y / 71) % 4) as usize];
                    for c in 0..3 {
                        assert!(
                            (i32::from(actual[c]) - expected[c]).abs() <= 1,
                            "pixel {x},{y}: {actual:?}"
                        );
                    }
                    assert_eq!(actual[3], expected_alpha[((x as u32 + y) % 4) as usize]);
                }
            }
            assert!(reader.next_row().unwrap().is_none());
            reader.finish().unwrap();
            let jpeg_path = directory.path().join("large.jpg");
            completed.store(0, Ordering::Relaxed);
            let jpeg_started = Instant::now();
            export_image(
                &device,
                &queue,
                &source,
                None,
                &BasicAdjustments::exposure(1.0).unwrap(),
                &jpeg_path,
                ExportFormat::Jpeg(JpegOptions {
                    quality: 100,
                    dither_seed: None,
                    ..Default::default()
                }),
                TilePlan::new(w, h, &device.limits(), 1024).unwrap(),
                &cancel,
                &completed,
            )
            .unwrap();
            assert_eq!(completed.load(Ordering::Relaxed), total);
            use image::ImageDecoder;
            let mut decoder = image::codecs::jpeg::JpegDecoder::new(std::io::BufReader::new(
                std::fs::File::open(&jpeg_path).unwrap(),
            ))
            .unwrap();
            assert_eq!(decoder.dimensions(), (w, h));
            assert_eq!(decoder.color_type(), image::ColorType::Rgb8);
            assert!(decoder.icc_profile().unwrap().is_some());
            assert!(decoder.exif_metadata().unwrap().is_none());
            let mut rgb = vec![0; decoder.total_bytes() as usize];
            decoder.read_image(&mut rgb).unwrap();
            for (i, actual) in rgb.as_chunks::<3>().0.iter().enumerate() {
                let (x, y) = (i as u32 % w, i as u32 / w);
                let level = levels[((x / 73 + y / 71) % 4) as usize] * 2.0;
                let alpha = alphas[((x + y) % 4) as usize];
                let expected = (filter_core::color::linear_to_srgb(level * alpha + 1.0 - alpha)
                    * 255.0)
                    .round() as i32;
                for channel in actual {
                    assert!(
                        (i32::from(*channel) - expected).abs() <= 4,
                        "JPEG pixel {x},{y}: {actual:?} vs {expected}"
                    );
                }
            }
            eprintln!(
                "Validated {} JPEG pixels across {total} tiles in {:.2?}",
                u64::from(w) * u64::from(h),
                jpeg_started.elapsed()
            );
            eprintln!(
                "Validated {} pixels across {total} tiles in {:.2?}",
                u64::from(w) * u64::from(h),
                started.elapsed()
            );
        });
    }
}
