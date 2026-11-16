//! Whole-preview histograms in output sRGB, before checkerboard or dithering.
use crate::{ImageBuffer, color, cpu_eval::BasicAdjustments};
use half::f16;

pub const BINS: usize = 256;
pub const CHANNELS: usize = 4; // R, G, B, encoded Rec.709 luminance
pub const LOW_OFFSET: usize = BINS * CHANNELS;
pub const HIGH_OFFSET: usize = LOW_OFFSET + CHANNELS;
pub const SAMPLED_OFFSET: usize = HIGH_OFFSET + CHANNELS;
pub const TRANSPARENT_OFFSET: usize = SAMPLED_OFFSET + 1;
pub const INVALID_OFFSET: usize = TRANSPARENT_OFFSET + 1;
pub const LOW_PIXELS_OFFSET: usize = INVALID_OFFSET + 1;
pub const HIGH_PIXELS_OFFSET: usize = LOW_PIXELS_OFFSET + 1;
// Pad to 16 bytes; keep the storage layout synchronized with histogram.wgsl.
pub const WORDS: usize = HIGH_PIXELS_OFFSET + 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Histogram {
    pub bins: [[u32; BINS]; CHANNELS],
    pub clipped_low: [u32; CHANNELS],
    pub clipped_high: [u32; CHANNELS],
    pub sampled: u32,
    pub transparent: u32,
    pub invalid: u32,
    /// Pixels with any RGB channel at an endpoint. A pixel may appear in both.
    pub low_pixels: u32,
    pub high_pixels: u32,
}
impl Default for Histogram {
    fn default() -> Self {
        Self {
            bins: [[0; BINS]; CHANNELS],
            clipped_low: [0; CHANNELS],
            clipped_high: [0; CHANNELS],
            sampled: 0,
            transparent: 0,
            invalid: 0,
            low_pixels: 0,
            high_pixels: 0,
        }
    }
}
impl Histogram {
    pub fn total_pixels(&self) -> u64 {
        u64::from(self.sampled) + u64::from(self.transparent) + u64::from(self.invalid)
    }

    pub fn from_words(words: &[u32]) -> Result<Self, &'static str> {
        if words.len() != WORDS {
            return Err("Invalid histogram buffer length");
        }
        let mut result = Self::default();
        for (channel, bins) in result.bins.iter_mut().enumerate() {
            bins.copy_from_slice(&words[channel * BINS..(channel + 1) * BINS]);
        }
        result
            .clipped_low
            .copy_from_slice(&words[LOW_OFFSET..HIGH_OFFSET]);
        result
            .clipped_high
            .copy_from_slice(&words[HIGH_OFFSET..SAMPLED_OFFSET]);
        result.sampled = words[SAMPLED_OFFSET];
        result.transparent = words[TRANSPARENT_OFFSET];
        result.invalid = words[INVALID_OFFSET];
        result.low_pixels = words[LOW_PIXELS_OFFSET];
        result.high_pixels = words[HIGH_PIXELS_OFFSET];
        if result.bins.iter().any(|bins| {
            bins.iter().map(|v| u64::from(*v)).sum::<u64>() != u64::from(result.sampled)
        }) || result
            .clipped_low
            .iter()
            .chain(&result.clipped_high)
            .any(|v| *v > result.sampled)
            || result.low_pixels > result.sampled
            || result.high_pixels > result.sampled
        {
            return Err("Inconsistent histogram counters");
        }
        Ok(result)
    }

    /// CPU reference for the GPU preview path. Reproduce its premultiplied f16
    /// upload and unpremultiplication before applying the canonical adjustments.
    /// Partial alpha counts once; hidden color and checkerboard never contribute.
    pub fn reference_preview(
        image: &ImageBuffer,
        settings: &BasicAdjustments,
    ) -> Result<Self, &'static str> {
        if image.pixels().len() > u32::MAX as usize {
            return Err("Histogram pixel count exceeds u32");
        }
        let mut result = Self::default();
        for p in image.pixels() {
            let alpha = p[3].to_f32();
            if alpha <= 0.0 {
                result.transparent += 1;
                continue;
            }
            let sampled = [
                f16::from_f32(p[0].to_f32() * alpha).to_f32(),
                f16::from_f32(p[1].to_f32() * alpha).to_f32(),
                f16::from_f32(p[2].to_f32() * alpha).to_f32(),
                alpha,
            ];
            if !sampled.iter().all(|v| v.is_finite()) {
                result.invalid += 1;
                continue;
            }
            let working = settings.pixel([
                sampled[0] / alpha.max(0.000001),
                sampled[1] / alpha.max(0.000001),
                sampled[2] / alpha.max(0.000001),
                alpha,
            ]);
            let rgb = color::rec2020_to_linear_srgb(working[..3].try_into().unwrap());
            if !rgb.iter().all(|v| v.is_finite()) {
                result.invalid += 1;
                continue;
            }
            result.add_output(rgb);
        }
        Ok(result)
    }

    fn add_output(&mut self, rgb: [f32; 3]) {
        let rgb = rgb.map(|v| v.clamp(0.0, 1.0));
        let luma = rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
        let values = [rgb[0], rgb[1], rgb[2], luma.clamp(0.0, 1.0)];
        for (channel, value) in values.into_iter().enumerate() {
            let bin = ((color::linear_to_srgb(value) * BINS as f32) as usize).min(BINS - 1);
            self.bins[channel][bin] += 1;
            self.clipped_low[channel] += u32::from(value <= 0.0);
            self.clipped_high[channel] += u32::from(value >= 1.0);
        }
        self.low_pixels += u32::from(rgb.iter().any(|v| *v <= 0.0));
        self.high_pixels += u32::from(rgb.iter().any(|v| *v >= 1.0));
        self.sampled += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_bins_luminance_and_channel_clipping_have_distinct_meanings() {
        let mut histogram = Histogram::default();
        for rgb in [[0.0; 3], [1.0; 3], [1.0, 0.0, 0.0], [0.25; 3]] {
            histogram.add_output(rgb);
        }
        assert_eq!(histogram.sampled, 4);
        assert_eq!(histogram.low_pixels, 2);
        assert_eq!(histogram.high_pixels, 2);
        assert_eq!(histogram.clipped_low, [1, 2, 2, 1]);
        assert_eq!(histogram.clipped_high, [2, 1, 1, 1]);
        assert_eq!(histogram.bins[0][255], 2);
        assert_eq!(histogram.bins[3][127], 1); // encoded red luminance, not encoded R
        assert_eq!(histogram.bins[3][137], 1); // encoded linear 0.25
    }
    #[test]
    fn hidden_nonfinite_color_is_ignored_and_partial_alpha_counts_once() {
        let image = ImageBuffer::new(
            4,
            1,
            vec![
                [f16::NAN, f16::INFINITY, f16::ONE, f16::ZERO],
                [
                    f16::from_f32(0.25),
                    f16::from_f32(0.25),
                    f16::from_f32(0.25),
                    f16::from_f32(0.5),
                ],
                [f16::NAN, f16::ONE, f16::ONE, f16::ONE],
                [f16::ZERO; 4],
            ],
        )
        .unwrap();
        let histogram = Histogram::reference_preview(&image, &BasicAdjustments::default()).unwrap();
        assert_eq!(
            (histogram.sampled, histogram.transparent, histogram.invalid),
            (1, 2, 1)
        );
        assert_eq!(histogram.total_pixels(), 4);
        assert!(histogram.bins.iter().all(|bins| bins[137] == 1));
    }
    #[test]
    fn readback_rejects_inconsistent_counts_and_accepts_empty_images() {
        assert_eq!(
            Histogram::from_words(&[0; WORDS]).unwrap(),
            Histogram::default()
        );
        assert!(Histogram::from_words(&[]).is_err());
        let mut words = [0; WORDS];
        words[SAMPLED_OFFSET] = 1;
        assert!(Histogram::from_words(&words).is_err());
        for c in 0..CHANNELS {
            words[c * BINS] = 1;
        }
        assert!(Histogram::from_words(&words).is_ok());
        words[LOW_PIXELS_OFFSET] = 2;
        assert!(Histogram::from_words(&words).is_err());
    }
}
