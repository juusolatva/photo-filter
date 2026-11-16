//! Bounded, post-adjustment area reduction in straight linear working space.
use anyhow::{Context, Result, ensure};
use half::f16;
use std::sync::atomic::{AtomicBool, Ordering};

type Pixel = [f16; 4];
const RESIZE_ROW_BYTES: u64 = (2 * size_of::<[f64; 4]>() + size_of::<Pixel>()) as u64;
const RESIZE_ROW_BUDGET: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExportResize {
    #[default]
    Full,
    LongEdge(u32),
    Percentage(u32),
}

impl ExportResize {
    /// Reduction only. Round to the nearest pixel, retaining at least one per axis.
    pub fn dimensions(self, width: u32, height: u32) -> Result<(u32, u32)> {
        ensure!(width > 0 && height > 0, "Invalid source dimensions");
        let (numerator, denominator) = match self {
            Self::Full => (1, 1),
            Self::LongEdge(edge) => {
                ensure!(edge > 0, "Long edge must be at least one pixel");
                (edge.min(width.max(height)), width.max(height))
            }
            Self::Percentage(percent) => {
                ensure!((1..=100).contains(&percent), "Percentage must be 1–100");
                (percent, 100)
            }
        };
        let scale = |size| {
            ((u64::from(size) * u64::from(numerator) + u64::from(denominator / 2))
                / u64::from(denominator))
            .max(1) as u32
        };
        Ok((scale(width), scale(height)))
    }
}

/// Consumes nonempty full-width rendered bands and emits one reduced row at a time.
/// Keeps one input band, two f64 output-width rows and one f16 output row.
/// Integer footprint boundaries make overlap weights independent of band/tile edges.
pub struct ResizeBands<'a, I> {
    bands: I,
    source: (u32, u32),
    target: (u32, u32),
    band: Vec<Pixel>,
    offset: usize,
    source_y: u32,
    cached_y: Option<u32>,
    horizontal: Vec<[f64; 4]>,
    sum: Vec<[f64; 4]>,
    output_y: u32,
    done: bool,
    cancel: &'a AtomicBool,
}

impl<'a, I: Iterator<Item = Result<Vec<Pixel>>>> ResizeBands<'a, I> {
    pub fn new(
        bands: I,
        source: (u32, u32),
        target: (u32, u32),
        cancel: &'a AtomicBool,
    ) -> Result<Self> {
        ensure!(
            target.0 > 0 && target.1 > 0 && target.0 <= source.0 && target.1 <= source.1,
            "Resize dimensions must be positive and cannot enlarge the source"
        );
        ensure!(
            u64::from(target.0) * RESIZE_ROW_BYTES <= RESIZE_ROW_BUDGET,
            "Resize row buffers exceed the 64 MiB limit"
        );
        check_cancel(cancel)?;
        let allocate = || -> Result<Vec<[f64; 4]>> {
            let mut row = Vec::new();
            row.try_reserve_exact(target.0 as usize)
                .context("Could not allocate resize row")?;
            row.resize(target.0 as usize, [0.0; 4]);
            Ok(row)
        };
        Ok(Self {
            bands,
            source,
            target,
            band: Vec::new(),
            offset: 0,
            source_y: 0,
            cached_y: None,
            horizontal: allocate()?,
            sum: allocate()?,
            output_y: 0,
            done: false,
            cancel,
        })
    }

    fn horizontal_row(&mut self, y: u32) -> Result<()> {
        if self.cached_y == Some(y) {
            return Ok(());
        }
        ensure!(y == self.source_y, "Out-of-order resize source row");
        if self.offset == self.band.len() {
            check_cancel(self.cancel)?;
            // Release the consumed band before requesting another GPU row band.
            self.band = Vec::new();
            self.offset = 0;
            self.band = self
                .bands
                .next()
                .context("Too few resize source pixels")??;
            ensure!(
                !self.band.is_empty()
                    && self.band.len().is_multiple_of(self.source.0 as usize)
                    && self.band.len() / self.source.0 as usize
                        <= (self.source.1 - self.source_y) as usize,
                "Invalid resize source band length"
            );
        }
        let row = &self.band[self.offset..self.offset + self.source.0 as usize];
        let sw = u64::from(self.source.0);
        let dw = u64::from(self.target.0);
        let mut taps = 0_u64;
        for (x, result) in self.horizontal.iter_mut().enumerate() {
            let start = x as u64 * sw;
            let end = start + sw;
            let mut sum = [0.0; 4];
            for ix in start / dw..end.div_ceil(dw) {
                if taps.is_multiple_of(4096) {
                    check_cancel(self.cancel)?;
                }
                taps += 1;
                let weight = (end.min((ix + 1) * dw) - start.max(ix * dw)) as f64 / sw as f64;
                let pixel = row[ix as usize].map(f16::to_f64);
                let alpha = if pixel[3].is_finite() {
                    pixel[3].clamp(0.0, 1.0)
                } else {
                    0.0
                };
                // Fully transparent hidden RGB, including NaN/Inf, cannot bleed into edges.
                if alpha > 0.0 {
                    for c in 0..3 {
                        if pixel[c].is_finite() {
                            sum[c] += pixel[c] * alpha * weight;
                        }
                    }
                    sum[3] += alpha * weight;
                }
            }
            *result = sum;
        }
        self.offset += self.source.0 as usize;
        self.source_y += 1;
        self.cached_y = Some(y);
        Ok(())
    }

    fn row(&mut self) -> Result<Vec<Pixel>> {
        check_cancel(self.cancel)?;
        self.sum.fill([0.0; 4]);
        let sh = u64::from(self.source.1);
        let dh = u64::from(self.target.1);
        let start = u64::from(self.output_y) * sh;
        let end = start + sh;
        for iy in start / dh..end.div_ceil(dh) {
            self.horizontal_row(iy as u32)?;
            let weight = (end.min((iy + 1) * dh) - start.max(iy * dh)) as f64 / sh as f64;
            for (x, (sum, row)) in self.sum.iter_mut().zip(&self.horizontal).enumerate() {
                if x.is_multiple_of(4096) {
                    check_cancel(self.cancel)?;
                }
                for c in 0..4 {
                    sum[c] += row[c] * weight;
                }
            }
        }
        let mut output = Vec::new();
        output
            .try_reserve_exact(self.target.0 as usize)
            .context("Could not allocate resized output row")?;
        for (x, sum) in self.sum.iter().enumerate() {
            if x.is_multiple_of(4096) {
                check_cancel(self.cancel)?;
            }
            output.push([
                f16::from_f64(if sum[3] > 0.0 { sum[0] / sum[3] } else { 0.0 }),
                f16::from_f64(if sum[3] > 0.0 { sum[1] / sum[3] } else { 0.0 }),
                f16::from_f64(if sum[3] > 0.0 { sum[2] / sum[3] } else { 0.0 }),
                f16::from_f64(sum[3].clamp(0.0, 1.0)),
            ]);
        }
        self.output_y += 1;
        Ok(output)
    }
}

impl<I: Iterator<Item = Result<Vec<Pixel>>>> Iterator for ResizeBands<'_, I> {
    type Item = Result<Vec<Pixel>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let result = if self.output_y < self.target.1 {
            self.row()
        } else {
            self.done = true;
            // Propagate late producer failures rather than publishing a partial render.
            let finish = (|| {
                check_cancel(self.cancel)?;
                ensure!(self.source_y == self.source.1, "Too few resize source rows");
                ensure!(
                    self.bands.next().transpose()?.is_none(),
                    "Unexpected resize source band after final row"
                );
                Ok(())
            })();
            return finish.err().map(Err);
        };
        if result.is_err() {
            self.done = true;
        }
        Some(result)
    }
}

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "Export cancelled");
    Ok(())
}

#[cfg(test)]
#[path = "../tests/unit/resize_tests.rs"]
mod tests;
