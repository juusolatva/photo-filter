use super::*;
#[test]
fn output_clips_before_encoding_and_lab_and_keeps_alpha_separate() {
    let sample = ProbeSample::from_linear_srgb([2.0, -1.0, 0.0, 0.25]);
    assert_eq!(sample.rgb8(), Some([255, 0, 0]));
    let ProbeSample::Color { oklab, alpha, .. } = sample else {
        panic!()
    };
    assert!((oklab[0] - 0.62796).abs() < 0.0001);
    assert!((oklab[1] - 0.22486).abs() < 0.0001);
    assert!((oklab[2] - 0.12585).abs() < 0.0001);
    assert_eq!(alpha, 0.25);
    assert_eq!(
        ProbeSample::from_linear_srgb([0.0, 0.0, 0.0, 1.0]).rgb8(),
        Some([0; 3])
    );
    assert_eq!(
        ProbeSample::from_linear_srgb([1.0; 4]).rgb8(),
        Some([255; 3])
    );
}
#[test]
fn hidden_color_is_not_reported_and_nonfinite_visible_samples_are_invalid() {
    let settings = BasicAdjustments::default();
    assert_eq!(
        ProbeSample::reference([f32::NAN, f32::INFINITY, 1.0, 0.0], &settings),
        ProbeSample::Transparent
    );
    for pixel in [[f32::NAN, 1.0, 1.0, 1.0], [1.0, 1.0, 1.0, f32::NAN]] {
        assert_eq!(
            ProbeSample::reference(pixel, &settings),
            ProbeSample::Invalid
        );
    }
    let full = ProbeSample::reference([0.25; 4], &settings);
    let partial = ProbeSample::reference([0.25, 0.25, 0.25, 1.0], &settings);
    assert_eq!(full.rgb8(), partial.rgb8());
}
