use anyhow::{Context, Result, ensure};
use filter_gpu::{TilePlan, TiledExposure};
use filter_io::DecodedImage;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

pub enum ExportResult {
    Saved(PathBuf),
    Cancelled,
    Failed(String),
}

pub struct ExportJob {
    pub ev: f32,
    total: u64,
    completed: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    result: mpsc::Receiver<ExportResult>,
}
impl ExportJob {
    pub fn start(
        device: wgpu::Device,
        queue: wgpu::Queue,
        source: Arc<DecodedImage>,
        ev: f32,
        path: PathBuf,
        ctx: eframe::egui::Context,
    ) -> Result<Self> {
        validate_png_path(&path)?;
        let plan = TilePlan::new(
            source.buffer.width(),
            source.buffer.height(),
            &device.limits(),
            1024,
        )?;
        let total = plan.tile_count();
        let completed = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, result) = mpsc::channel();
        let job = Self {
            ev,
            total,
            completed: completed.clone(),
            cancel: cancel.clone(),
            result,
        };
        std::thread::Builder::new()
            .name("png-export".into())
            .spawn(move || {
                let result = export_png(
                    &device,
                    &queue,
                    &source.buffer,
                    ev,
                    &path,
                    plan,
                    &cancel,
                    &completed,
                );
                let message = match result {
                    Ok(()) => ExportResult::Saved(path),
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
fn export_png(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    source: &filter_core::ImageBuffer,
    ev: f32,
    path: &std::path::Path,
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
                let mut pending = renderer.submit(device, queue, source, tile, ev)?;
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
        filter_io::write_srgb16_png(file, source.width(), source.height(), bands)
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

fn validate_png_path(path: &std::path::Path) -> Result<()> {
    ensure!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("png")),
        "PNG export requires a filename ending in .png"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn export_requires_png_extension_without_changing_selected_destination() {
        assert!(validate_png_path(std::path::Path::new("photo.png")).is_ok());
        assert!(validate_png_path(std::path::Path::new("photo.PNG")).is_ok());
        assert!(validate_png_path(std::path::Path::new("photo.jpg")).is_err());
        assert!(validate_png_path(std::path::Path::new("photo")).is_err());
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
    #[ignore = "requires a software GPU and about 400 MiB RAM; exercises >36MP PNG export"]
    fn large_png_export_exceeds_texture_and_full_readback_limits() {
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
            export_png(
                &device, &queue, &source, 1.0, &path, plan, &cancel, &completed,
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
            eprintln!(
                "Validated {} pixels across {total} tiles in {:.2?}",
                u64::from(w) * u64::from(h),
                started.elapsed()
            );
        });
    }
}
