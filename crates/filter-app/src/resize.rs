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

/// Consumes full-width rendered bands and emits one reduced row at a time.
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
        while self.offset == self.band.len() {
            check_cancel(self.cancel)?;
            // Release the consumed band before requesting another GPU row band.
            self.band = Vec::new();
            self.offset = 0;
            self.band = self
                .bands
                .next()
                .context("Too few resize source pixels")??;
            ensure!(
                self.band.len().is_multiple_of(self.source.0 as usize)
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
                for band in self.bands.by_ref() {
                    check_cancel(self.cancel)?;
                    ensure!(band?.is_empty(), "Too many resize source pixels");
                }
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
mod tests {
    use super::*;

    #[test]
    fn dimensions_round_preserve_thin_images_and_never_enlarge() {
        for (setting, source, expected) in [
            (ExportResize::Full, (53, 11), (53, 11)),
            (ExportResize::LongEdge(20), (53, 11), (20, 4)),
            (ExportResize::LongEdge(20), (11, 53), (4, 20)),
            (ExportResize::LongEdge(100), (53, 11), (53, 11)),
            (ExportResize::LongEdge(1), (1, 10000), (1, 1)),
            (ExportResize::Percentage(50), (53, 11), (27, 6)),
            (ExportResize::Percentage(1), (1, 10000), (1, 100)),
            (ExportResize::Percentage(100), (53, 11), (53, 11)),
            (
                ExportResize::LongEdge(u32::MAX - 1),
                (u32::MAX, 1),
                (u32::MAX - 1, 1),
            ),
        ] {
            assert_eq!(setting.dimensions(source.0, source.1).unwrap(), expected);
        }
        for setting in [
            ExportResize::LongEdge(0),
            ExportResize::Percentage(0),
            ExportResize::Percentage(101),
        ] {
            assert!(setting.dimensions(10, 10).is_err());
        }
        assert!(ExportResize::Full.dimensions(0, 1).is_err());
    }

    // Independent direct 2D area integration, with no row cache or band iteration.
    fn reference(pixels: &[Pixel], source: (u32, u32), target: (u32, u32)) -> Vec<Pixel> {
        let sx = f64::from(source.0) / f64::from(target.0);
        let sy = f64::from(source.1) / f64::from(target.1);
        let mut output = Vec::new();
        for y in 0..target.1 {
            for x in 0..target.0 {
                let x0 = f64::from(x) * sx;
                let x1 = f64::from(x + 1) * sx;
                let y0 = f64::from(y) * sy;
                let y1 = f64::from(y + 1) * sy;
                let mut sum = [0.0; 4];
                for iy in y0.floor() as u32..(y1.ceil() as u32).min(source.1) {
                    for ix in x0.floor() as u32..(x1.ceil() as u32).min(source.0) {
                        let weight = (x1.min(f64::from(ix + 1)) - x0.max(f64::from(ix)))
                            * (y1.min(f64::from(iy + 1)) - y0.max(f64::from(iy)));
                        let p = pixels[(iy * source.0 + ix) as usize].map(f16::to_f64);
                        for c in 0..3 {
                            sum[c] += p[c] * p[3] * weight;
                        }
                        sum[3] += p[3] * weight;
                    }
                }
                output.push([
                    f16::from_f64(if sum[3] > 0.0 { sum[0] / sum[3] } else { 0.0 }),
                    f16::from_f64(if sum[3] > 0.0 { sum[1] / sum[3] } else { 0.0 }),
                    f16::from_f64(if sum[3] > 0.0 { sum[2] / sum[3] } else { 0.0 }),
                    f16::from_f64(sum[3] / (sx * sy)),
                ]);
            }
        }
        output
    }

    #[test]
    fn area_reduction_matches_reference_and_is_band_independent() {
        for (source, target) in [
            ((53, 11), (27, 6)),
            ((7, 5), (6, 4)),
            ((1009, 89), (13, 7)),
            ((1, 101), (1, 3)),
            ((101, 1), (3, 1)),
            ((3, 17), (3, 2)),
            ((17, 3), (2, 3)),
        ] {
            let pixels: Vec<Pixel> = (0..source.0 * source.1)
                .map(|i| {
                    [
                        ((i * 37 % 101) as f32 - 20.0) / 32.0,
                        (i % 31) as f32 / 32.0,
                        0.25,
                        (i % 5) as f32 / 4.0,
                    ]
                    .map(f16::from_f32)
                })
                .collect();
            let expected = reference(&pixels, source, target);
            let mut previous = None;
            for band_rows in [1, 3, 17, source.1] {
                let cancel = AtomicBool::new(false);
                let bands = pixels
                    .chunks(source.0 as usize * band_rows as usize)
                    .map(|p| Ok(p.to_vec()));
                let resize = ResizeBands::new(bands, source, target, &cancel).unwrap();
                assert_eq!(resize.horizontal.len(), target.0 as usize);
                assert_eq!(resize.sum.len(), target.0 as usize);
                let actual: Vec<_> = resize
                    .collect::<Result<Vec<_>>>()
                    .unwrap()
                    .into_iter()
                    .flatten()
                    .collect();
                assert_eq!(actual.len(), expected.len());
                for (a, e) in actual.iter().zip(&expected) {
                    for c in 0..4 {
                        assert!(
                            (a[c].to_f32() - e[c].to_f32()).abs()
                                <= 0.001 * (1.0 + e[c].to_f32().abs()),
                            "{a:?} vs {e:?}"
                        );
                    }
                }
                if let Some(previous) = previous {
                    assert_eq!(actual, previous);
                }
                previous = Some(actual);
            }
        }
    }

    #[test]
    fn reduction_is_linear_and_alpha_weighted_with_defined_nonfinite_handling() {
        let cancel = AtomicBool::new(false);
        for (pixels, expected) in [
            (vec![[1.0; 4], [0.0, 0.0, 0.0, 1.0]], [0.5, 0.5, 0.5, 1.0]),
            (vec![[1.0; 4], [0.0; 4]], [1.0, 1.0, 1.0, 0.5]),
            (
                vec![[1.0; 4], [f32::NAN, f32::INFINITY, 999.0, 0.0]],
                [1.0, 1.0, 1.0, 0.5],
            ),
            (
                vec![[1.0; 4], [1.0, 1.0, 1.0, f32::NAN]],
                [1.0, 1.0, 1.0, 0.5],
            ),
            (
                vec![[f32::NAN, f32::INFINITY, -1.0, 1.0], [0.0; 4]],
                [0.0, 0.0, -1.0, 0.5],
            ),
            (
                vec![[123.0, 23.0, 2.0, 0.0], [1.0, 2.0, 3.0, 0.0]],
                [0.0; 4],
            ),
        ] {
            let bands = [Ok(pixels
                .into_iter()
                .map(|p| p.map(f16::from_f32))
                .collect())]
            .into_iter();
            let actual: Vec<_> = ResizeBands::new(bands, (2, 1), (1, 1), &cancel)
                .unwrap()
                .collect::<Result<Vec<_>>>()
                .unwrap();
            assert_eq!(actual[0][0].map(f16::to_f32), expected);
        }
    }

    #[test]
    fn invalid_streams_limits_producer_errors_and_cancellation_fail() {
        let cancel = AtomicBool::new(false);
        for bands in [
            vec![],
            vec![Ok(vec![[f16::ONE; 4]; 3])],
            vec![Ok(vec![[f16::ONE; 4]; 16])],
            vec![Ok(vec![[f16::ONE; 4]; 4])],
            vec![Err(anyhow::anyhow!("producer failed"))],
            vec![
                Ok(vec![[f16::ONE; 4]; 8]),
                Err(anyhow::anyhow!("late producer failure")),
            ],
            vec![Ok(vec![[f16::ONE; 4]; 8]), Ok(vec![[f16::ONE; 4]; 4])],
        ] {
            let mut resize = ResizeBands::new(bands.into_iter(), (4, 2), (2, 1), &cancel).unwrap();
            assert!(resize.by_ref().collect::<Result<Vec<_>>>().is_err());
            assert!(resize.next().is_none(), "errors terminate the stream");
        }
        for target in [(0, 1), (1_000_001, 1), (1, 3), (1_000_000, 1)] {
            assert!(ResizeBands::new(std::iter::empty(), (1_000_000, 2), target, &cancel).is_err());
        }
        let bands = [Ok(vec![[f16::ONE; 4]; 8])].into_iter();
        let mut resize = ResizeBands::new(bands, (2, 4), (1, 2), &cancel).unwrap();
        assert!(resize.next().unwrap().is_ok());
        cancel.store(true, Ordering::Relaxed);
        assert!(resize.next().unwrap().is_err());
        assert!(resize.next().is_none());
        assert!(ResizeBands::new(std::iter::empty(), (4, 2), (2, 1), &cancel).is_err());
    }
}
