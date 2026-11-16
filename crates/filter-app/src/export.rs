use crate::resize::{ExportResize, ResizeBands};
use anyhow::{Context, Result, ensure};
use filter_core::cpu_eval::BasicAdjustments;
use filter_gpu::{TilePlan, TiledExposure};
use filter_io::{
    DecodedImage, JpegOptions, PngBitDepth, PngOptions, TiffBitDepth, TiffOptions, WebPOptions,
};
use std::io::{Seek, Write};
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
    Png(PngOptions),
    Jpeg(JpegOptions),
    WebP(WebPOptions),
    Tiff(TiffOptions),
}
impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png(_) => "png",
            Self::Jpeg(_) => "jpg",
            Self::WebP(_) => "webp",
            Self::Tiff(_) => "tiff",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Png(options) => match options.bit_depth {
                PngBitDepth::Eight => "8-bit PNG",
                PngBitDepth::Sixteen => "16-bit PNG",
            },
            Self::Jpeg(_) => "JPEG",
            Self::WebP(_) => "Lossless WebP",
            Self::Tiff(options) => match options.bit_depth {
                TiffBitDepth::Eight => "8-bit TIFF",
                TiffBitDepth::Sixteen => "16-bit TIFF",
            },
        }
    }
    fn validate_path(self, path: &std::path::Path) -> Result<()> {
        let extension = path
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or_default();
        let valid = match self {
            Self::Png(_) => extension.eq_ignore_ascii_case("png"),
            Self::WebP(_) => extension.eq_ignore_ascii_case("webp"),
            Self::Tiff(_) => {
                extension.eq_ignore_ascii_case("tif") || extension.eq_ignore_ascii_case("tiff")
            }
            Self::Jpeg(_) => {
                extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg")
            }
        };
        ensure!(
            valid,
            "{} export requires a filename ending in {}",
            self.label(),
            match self {
                Self::Png(_) => ".png",
                Self::Jpeg(_) => ".jpg or .jpeg",
                Self::WebP(_) => ".webp",
                Self::Tiff(_) => ".tif or .tiff",
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
        resize: ExportResize,
        ctx: eframe::egui::Context,
    ) -> Result<Self> {
        format.validate_path(&path)?;
        resize.dimensions(source.buffer.width(), source.buffer.height())?;
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
                    resize,
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
    resize: ExportResize,
) -> Result<()> {
    let dimensions = resize.dimensions(source.width(), source.height())?;
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
    let bands: Box<dyn Iterator<Item = Result<Vec<[half::f16; 4]>>>> =
        if dimensions == (source.width(), source.height()) {
            Box::new(bands)
        } else {
            Box::new(ResizeBands::new(
                bands,
                (source.width(), source.height()),
                dimensions,
                cancel,
            )?)
        };
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
            ExportFormat::Png(options) => filter_io::write_srgb_png_cancellable(
                &mut output,
                dimensions.0,
                dimensions.1,
                exif,
                options,
                bands,
                || cancel.load(Ordering::Relaxed),
            )?,
            ExportFormat::Jpeg(options) => filter_io::write_srgb8_jpeg_cancellable(
                &mut output,
                dimensions.0,
                dimensions.1,
                exif,
                options,
                bands,
                || cancel.load(Ordering::Relaxed),
            )?,
            ExportFormat::WebP(options) => filter_io::write_srgb8_webp_cancellable(
                &mut output,
                dimensions.0,
                dimensions.1,
                exif,
                options,
                bands,
                || cancel.load(Ordering::Relaxed),
            )?,
            ExportFormat::Tiff(options) => filter_io::write_srgb_tiff_cancellable(
                &mut output,
                dimensions.0,
                dimensions.1,
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
impl<W: Seek> Seek for CancelWriter<'_, W> {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("Export cancelled"));
        }
        self.output.seek(position)
    }
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

    #[test]
    #[ignore = "requires a software GPU"]
    fn image_exports_render_the_captured_basic_adjustments() {
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
                ExportFormat::Png(PngOptions::default()),
                plan,
                &AtomicBool::new(false),
                &completed,
                ExportResize::Full,
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
            for tile_edge in [17, 31] {
                let tiff_path = directory.path().join("adjusted16.tiff");
                completed.store(0, Ordering::Relaxed);
                let plan = TilePlan::new(w, h, &device.limits(), tile_edge).unwrap();
                let tile_count = plan.tile_count();
                export_image(
                    &device,
                    &queue,
                    &image,
                    Some(&exif),
                    &snapshot,
                    &tiff_path,
                    ExportFormat::Tiff(TiffOptions::default()),
                    plan,
                    &AtomicBool::new(false),
                    &completed,
                    ExportResize::Full,
                )
                .unwrap();
                assert_eq!(completed.load(Ordering::Relaxed), tile_count);
                let reloaded = filter_io::decode(&tiff_path).unwrap();
                assert!(reloaded.icc_profile.is_some());
                assert!(reloaded.exif_bytes.is_none());
                assert_eq!(
                    reloaded.source_orientation,
                    image::metadata::Orientation::NoTransforms
                );
                assert_eq!(
                    image::open(&tiff_path).unwrap().into_rgba16(),
                    image::open(directory.path().join("adjusted.png"))
                        .unwrap()
                        .into_rgba16()
                );
            }
            for seed in [None, Some(0)] {
                let options = PngOptions {
                    bit_depth: PngBitDepth::Eight,
                    dither_seed: seed,
                };
                let mut previous = None;
                for tile_edge in [17, 31] {
                    let path = directory.path().join("adjusted8.png");
                    completed.store(0, Ordering::Relaxed);
                    let plan = TilePlan::new(w, h, &device.limits(), tile_edge).unwrap();
                    let count = plan.tile_count();
                    export_image(
                        &device,
                        &queue,
                        &image,
                        Some(&exif),
                        &snapshot,
                        &path,
                        ExportFormat::Png(options),
                        plan,
                        &AtomicBool::new(false),
                        &completed,
                        ExportResize::Full,
                    )
                    .unwrap();
                    assert_eq!(completed.load(Ordering::Relaxed), count);
                    let reloaded = filter_io::decode(&path).unwrap();
                    assert_eq!(
                        reloaded.exif_bytes.as_deref(),
                        Some(expected_exif.as_slice())
                    );
                    assert_eq!(
                        reloaded.source_orientation,
                        image::metadata::Orientation::NoTransforms
                    );
                    let actual = image::open(&path).unwrap();
                    assert_eq!(actual.color(), image::ColorType::Rgba8);
                    let actual = actual.into_rgba8();
                    let webp_path = directory.path().join("adjusted.webp");
                    completed.store(0, Ordering::Relaxed);
                    export_image(
                        &device,
                        &queue,
                        &image,
                        Some(&exif),
                        &snapshot,
                        &webp_path,
                        ExportFormat::WebP(WebPOptions { dither_seed: seed }),
                        TilePlan::new(w, h, &device.limits(), tile_edge).unwrap(),
                        &AtomicBool::new(false),
                        &completed,
                        ExportResize::Full,
                    )
                    .unwrap();
                    assert_eq!(completed.load(Ordering::Relaxed), count);
                    let reloaded_webp = filter_io::decode(&webp_path).unwrap();
                    assert_eq!(
                        reloaded_webp.exif_bytes.as_deref(),
                        Some(expected_exif.as_slice())
                    );
                    assert_eq!(
                        reloaded_webp.source_orientation,
                        image::metadata::Orientation::NoTransforms
                    );
                    assert!(reloaded_webp.icc_profile.is_some());
                    assert_eq!(
                        image::open(&webp_path).unwrap().into_rgba8(),
                        actual,
                        "lossless WebP must preserve the same quantized pixels as PNG8"
                    );
                    let tiff_path = directory.path().join("adjusted8.tif");
                    completed.store(0, Ordering::Relaxed);
                    // TIFF deliberately omits EXIF, even when the captured bytes are malformed.
                    export_image(
                        &device,
                        &queue,
                        &image,
                        Some(b"invalid EXIF"),
                        &snapshot,
                        &tiff_path,
                        ExportFormat::Tiff(TiffOptions {
                            bit_depth: TiffBitDepth::Eight,
                            dither_seed: seed,
                        }),
                        TilePlan::new(w, h, &device.limits(), tile_edge).unwrap(),
                        &AtomicBool::new(false),
                        &completed,
                        ExportResize::Full,
                    )
                    .unwrap();
                    assert_eq!(completed.load(Ordering::Relaxed), count);
                    assert_eq!(image::open(&tiff_path).unwrap().into_rgba8(), actual);
                    for (original, actual) in image.pixels().iter().zip(actual.pixels()) {
                        let working = snapshot
                            .pixel(original.map(half::f16::to_f32))
                            .map(|v| half::f16::from_f32(v).to_f32());
                        let rgb = filter_core::color::rec2020_to_linear_srgb(
                            working[..3].try_into().unwrap(),
                        );
                        for c in 0..3 {
                            let expected =
                                filter_core::color::linear_to_srgb(rgb[c]).clamp(0.0, 1.0) * 255.0;
                            // Up to 1.5 steps of dither/rounding plus CPU/GPU half-float error.
                            assert!((f32::from(actual[c]) - expected).abs() <= 2.0);
                        }
                        assert_eq!(actual[3], (working[3] * 255.0).round() as u8);
                    }
                    if let Some(previous) = previous {
                        assert_eq!(
                            actual, previous,
                            "tile boundaries must not affect dithering"
                        );
                    }
                    previous = Some(actual);
                }
            }
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
                ExportResize::Full,
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
            // Resize after the captured nonlinear adjustments, across formats and tile boundaries.
            let adjusted_source = filter_core::ImageBuffer::new(
                w,
                h,
                image
                    .pixels()
                    .iter()
                    .map(|p| {
                        snapshot
                            .pixel(p.map(half::f16::to_f32))
                            .map(half::f16::from_f32)
                    })
                    .collect(),
            )
            .unwrap();
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
                ExportFormat::Jpeg(options),
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
                let (ow, oh) = resize.dimensions(w, h).unwrap();
                let expected = adjusted_source.proxy(ow.max(oh));
                assert_eq!((expected.width(), expected.height()), (ow, oh));
                for (index, format) in formats.into_iter().enumerate() {
                    let mut previous = None;
                    for edge in [7, 17, 31] {
                        let path = directory
                            .path()
                            .join(format!("resized.{}", format.extension()));
                        let plan = TilePlan::new(w, h, &device.limits(), edge).unwrap();
                        let tile_count = plan.tile_count();
                        completed.store(0, Ordering::Relaxed);
                        export_image(
                            &device,
                            &queue,
                            &image,
                            Some(&exif),
                            &snapshot,
                            &path,
                            format,
                            plan,
                            &AtomicBool::new(false),
                            &completed,
                            resize,
                        )
                        .unwrap();
                        assert_eq!(completed.load(Ordering::Relaxed), tile_count);
                        let pixels = image::open(&path).unwrap().into_rgba16();
                        if let Some(previous) = previous {
                            assert_eq!(pixels, previous, "tile seams changed resized output");
                        }
                        previous = Some(pixels.clone());
                        if edge == 7 && resize == ExportResize::Full {
                            full_outputs.push(pixels.clone());
                        }
                        if matches!(
                            resize,
                            ExportResize::Percentage(100) | ExportResize::LongEdge(1000)
                        ) {
                            assert_eq!(
                                pixels, full_outputs[index],
                                "identity size must bypass filtering"
                            );
                        }
                        let actual = image::open(&path).unwrap();
                        assert_eq!((actual.width(), actual.height()), (ow, oh));
                        let reloaded = filter_io::decode(&path).unwrap();
                        assert!(reloaded.icc_profile.is_some());
                        assert_eq!(
                            reloaded.source_orientation,
                            image::metadata::Orientation::NoTransforms
                        );
                        if !matches!(format, ExportFormat::Tiff(_)) {
                            assert_eq!(
                                reloaded.exif_bytes.as_deref(),
                                Some(expected_exif.as_slice())
                            );
                        }
                        let expected_path = directory
                            .path()
                            .join(format!("expected.{}", format.extension()));
                        let output = std::fs::File::create(&expected_path).unwrap();
                        let bands = [Ok(expected.pixels().to_vec())];
                        match format {
                            ExportFormat::Png(options) => filter_io::write_srgb_png(
                                output,
                                ow,
                                oh,
                                Some(&exif),
                                options,
                                bands,
                            ),
                            ExportFormat::Jpeg(options) => filter_io::write_srgb8_jpeg(
                                output,
                                ow,
                                oh,
                                Some(&exif),
                                options,
                                bands,
                            ),
                            ExportFormat::WebP(options) => filter_io::write_srgb8_webp(
                                output,
                                ow,
                                oh,
                                Some(&exif),
                                options,
                                bands,
                            ),
                            ExportFormat::Tiff(options) => {
                                filter_io::write_srgb_tiff(output, ow, oh, options, bands)
                            }
                        }
                        .unwrap();
                        let expected_output = image::open(&expected_path).unwrap();
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
                            for (a, e) in actual
                                .into_rgba16()
                                .pixels()
                                .zip(expected_output.into_rgba16().pixels())
                            {
                                for c in 0..4 {
                                    assert!(
                                        (i32::from(a[c]) - i32::from(e[c])).abs() <= 128,
                                        "{resize:?} {format:?}: {a:?} vs {e:?}"
                                    );
                                }
                            }
                        } else {
                            let tolerance = if matches!(format, ExportFormat::Jpeg(_)) {
                                4
                            } else {
                                2
                            };
                            for (a, e) in actual
                                .into_rgba8()
                                .pixels()
                                .zip(expected_output.into_rgba8().pixels())
                            {
                                for c in 0..4 {
                                    assert!(
                                        (i16::from(a[c]) - i16::from(e[c])).abs() <= tolerance,
                                        "{resize:?} {format:?}: {a:?} vs {e:?}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    #[test]
    #[ignore = "requires a software GPU and about 800 MiB RAM; exercises >36MP image exports"]
    fn large_image_exports_exceed_texture_and_full_readback_limits() {
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
                ExportFormat::Png(PngOptions::default()),
                plan,
                &cancel,
                &completed,
                ExportResize::Full,
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
            let png8_started = Instant::now();
            let png8_path = directory.path().join("large8.png");
            completed.store(0, Ordering::Relaxed);
            export_image(
                &device,
                &queue,
                &source,
                None,
                &BasicAdjustments::exposure(1.0).unwrap(),
                &png8_path,
                ExportFormat::Png(PngOptions {
                    bit_depth: PngBitDepth::Eight,
                    ..Default::default()
                }),
                TilePlan::new(w, h, &device.limits(), 1024).unwrap(),
                &cancel,
                &completed,
                ExportResize::Full,
            )
            .unwrap();
            assert_eq!(completed.load(Ordering::Relaxed), total);
            let mut reader = png::Decoder::new(std::io::BufReader::new(
                std::fs::File::open(&png8_path).unwrap(),
            ))
            .read_info()
            .unwrap();
            assert_eq!((reader.info().width, reader.info().height), (w, h));
            assert_eq!(
                reader.output_color_type(),
                (png::ColorType::Rgba, png::BitDepth::Eight)
            );
            assert!(reader.info().icc_profile.is_some());
            let expected_rgb = levels.map(|v| {
                filter_core::color::rec2020_to_linear_srgb([v * 2.0; 3])
                    .map(|c| filter_core::color::linear_to_srgb(c).clamp(0.0, 1.0) * 255.0)
            });
            let expected_alpha = alphas.map(|v| (v * 255.0).round() as u8);
            for y in 0..h {
                let row = reader.next_row().unwrap().expect("missing PNG8 row");
                assert_eq!(row.data().len(), w as usize * 4);
                for (x, actual) in row.data().as_chunks::<4>().0.iter().enumerate() {
                    let expected = expected_rgb[((x as u32 / 73 + y / 71) % 4) as usize];
                    for c in 0..3 {
                        assert!(
                            (f32::from(actual[c]) - expected[c]).abs() <= 1.501,
                            "PNG8 pixel {x},{y}: {actual:?} vs {expected:?}"
                        );
                    }
                    assert_eq!(actual[3], expected_alpha[((x as u32 + y) % 4) as usize]);
                }
            }
            assert!(reader.next_row().unwrap().is_none());
            reader.finish().unwrap();
            eprintln!(
                "Validated {} PNG8 pixels across {total} tiles in {:.2?}",
                u64::from(w) * u64::from(h),
                png8_started.elapsed()
            );
            let webp_path = directory.path().join("large.webp");
            completed.store(0, Ordering::Relaxed);
            let webp_started = Instant::now();
            export_image(
                &device,
                &queue,
                &source,
                None,
                &BasicAdjustments::exposure(1.0).unwrap(),
                &webp_path,
                ExportFormat::WebP(WebPOptions::default()),
                TilePlan::new(w, h, &device.limits(), 1024).unwrap(),
                &cancel,
                &completed,
                ExportResize::Full,
            )
            .unwrap();
            assert_eq!(completed.load(Ordering::Relaxed), total);
            use image::ImageDecoder;
            let mut decoder = image::codecs::webp::WebPDecoder::new(std::io::BufReader::new(
                std::fs::File::open(&webp_path).unwrap(),
            ))
            .unwrap();
            assert_eq!(decoder.dimensions(), (w, h));
            assert_eq!(decoder.color_type(), image::ColorType::Rgba8);
            assert!(decoder.icc_profile().unwrap().is_some());
            assert!(decoder.exif_metadata().unwrap().is_none());
            let rgba = image::DynamicImage::from_decoder(decoder)
                .unwrap()
                .into_rgba8();
            let mut png_reader = png::Decoder::new(std::io::BufReader::new(
                std::fs::File::open(&png8_path).unwrap(),
            ))
            .read_info()
            .unwrap();
            for row in rgba.as_raw().chunks_exact(w as usize * 4) {
                assert_eq!(row, png_reader.next_row().unwrap().unwrap().data());
            }
            assert!(png_reader.next_row().unwrap().is_none());
            png_reader.finish().unwrap();
            drop(rgba);
            eprintln!(
                "Validated {} lossless WebP pixels across {total} tiles in {:.2?}",
                u64::from(w) * u64::from(h),
                webp_started.elapsed()
            );
            for bit_depth in [TiffBitDepth::Eight, TiffBitDepth::Sixteen] {
                let tiff_path = directory.path().join("large.tiff");
                std::fs::write(&tiff_path, b"old destination").unwrap();
                let tiff_started = Instant::now();
                completed.store(0, Ordering::Relaxed);
                export_image(
                    &device,
                    &queue,
                    &source,
                    None,
                    &BasicAdjustments::exposure(1.0).unwrap(),
                    &tiff_path,
                    ExportFormat::Tiff(TiffOptions {
                        bit_depth,
                        ..Default::default()
                    }),
                    TilePlan::new(w, h, &device.limits(), 1024).unwrap(),
                    &cancel,
                    &completed,
                    ExportResize::Full,
                )
                .unwrap();
                assert_eq!(completed.load(Ordering::Relaxed), total);
                // Read one TIFF strip and PNG row at a time; never a full output image.
                let mut decoder = tiff::decoder::Decoder::new(std::io::BufReader::new(
                    std::fs::File::open(&tiff_path).unwrap(),
                ))
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
                let reference = match bit_depth {
                    TiffBitDepth::Eight => &png8_path,
                    TiffBitDepth::Sixteen => &path,
                };
                let mut reader = png::Decoder::new(std::io::BufReader::new(
                    std::fs::File::open(reference).unwrap(),
                ))
                .read_info()
                .unwrap();
                let mut rows = 0;
                for strip in 0..decoder.strip_count().unwrap() {
                    match decoder.read_chunk(strip).unwrap() {
                        tiff::decoder::DecodingResult::U8(samples) => {
                            assert_eq!(bit_depth, TiffBitDepth::Eight);
                            for row in samples.chunks_exact(w as usize * 4) {
                                assert_eq!(row, reader.next_row().unwrap().unwrap().data());
                                rows += 1;
                            }
                        }
                        tiff::decoder::DecodingResult::U16(samples) => {
                            assert_eq!(bit_depth, TiffBitDepth::Sixteen);
                            for row in samples.chunks_exact(w as usize * 4) {
                                let expected = reader.next_row().unwrap().unwrap();
                                for (actual, expected) in
                                    row.iter().zip(expected.data().as_chunks::<2>().0)
                                {
                                    assert_eq!(*actual, u16::from_be_bytes(*expected));
                                }
                                rows += 1;
                            }
                        }
                        other => panic!("unexpected TIFF samples: {other:?}"),
                    }
                }
                assert_eq!(rows, h);
                assert!(reader.next_row().unwrap().is_none());
                reader.finish().unwrap();
                eprintln!(
                    "Validated {} {bit_depth:?}-bit TIFF pixels across {total} tiles in {:.2?}",
                    u64::from(w) * u64::from(h),
                    tiff_started.elapsed()
                );
            }
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
                ExportResize::Full,
            )
            .unwrap();
            assert_eq!(completed.load(Ordering::Relaxed), total);
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
            // Heavy reduction still renders every source tile with bounded row state.
            drop(rgb);
            let resize_started = Instant::now();
            let resized_path = directory.path().join("large-resized.png");
            completed.store(0, Ordering::Relaxed);
            export_image(
                &device,
                &queue,
                &source,
                None,
                &BasicAdjustments::exposure(1.0).unwrap(),
                &resized_path,
                ExportFormat::Png(PngOptions::default()),
                TilePlan::new(w, h, &device.limits(), 1024).unwrap(),
                &cancel,
                &completed,
                ExportResize::LongEdge(257),
            )
            .unwrap();
            assert_eq!(completed.load(Ordering::Relaxed), total);
            let expected = source.proxy(257);
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
                image::open(&resized_path).unwrap().into_rgba16(),
                image::load_from_memory(&encoded).unwrap().into_rgba16()
            );
            eprintln!(
                "Validated 36.6MP reduction to {} × {} across {total} tiles in {:.2?}",
                expected.width(),
                expected.height(),
                resize_started.elapsed()
            );
            eprintln!(
                "Validated {} pixels across {total} tiles in {:.2?}",
                u64::from(w) * u64::from(h),
                started.elapsed()
            );
        });
    }
}
