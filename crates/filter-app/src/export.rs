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
#[path = "../tests/unit/export_tests.rs"]
mod tests;
