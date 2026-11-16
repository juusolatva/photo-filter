use super::{
    BINS, BasicAdjustments, CHANNELS, Histogram, ImageBuffer, LOW_PIXELS_OFFSET, SAMPLED_OFFSET,
    WORDS, f16,
};
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
