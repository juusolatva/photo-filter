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
                    &device, &queue, &source, ev, &path, plan, &cancel, &completed,
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
    source: &DecodedImage,
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
            let width = source.buffer.width() as usize;
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
                let mut pending = renderer.submit(device, queue, &source.buffer, tile, ev)?;
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
        filter_io::write_srgb16_png(file, source.buffer.width(), source.buffer.height(), bands)
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
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .context("Could not create export temporary file")?;
    write(temporary.as_file_mut())?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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
}
