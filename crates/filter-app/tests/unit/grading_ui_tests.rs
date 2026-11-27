use super::*;

#[test]
fn hue_text_is_degrees_and_round_trips() {
    assert_eq!(degrees_text(0.0), "0°");
    assert_eq!(degrees_text(0.5), "180°");
    assert_eq!(degrees_text(1.0), "360°");
    assert_eq!(parse_degrees(" 90 "), Some(0.25));
    assert_eq!(parse_degrees("180°"), Some(0.5));
    assert_eq!(parse_degrees("720"), Some(1.0));
    assert_eq!(parse_degrees("-10"), Some(0.0));
    assert_eq!(parse_degrees("nan"), None);
    assert_eq!(parse_degrees("x"), None);
}
#[test]
fn chroma_text_is_percent_clamped_to_the_range() {
    assert_eq!(chroma_text(0.25), "25");
    assert_eq!(parse_chroma("40"), Some(0.4));
    assert_eq!(parse_chroma("-5"), Some(0.0));
    assert_eq!(parse_chroma("500"), Some(1.0));
    assert_eq!(parse_chroma("inf"), None);
}
#[test]
fn wheel_activity_includes_hue_alone() {
    let mut wheel = GradingWheel::default();
    assert!(!wheel_active(&wheel));
    wheel.hue = 0.2;
    assert!(wheel_active(&wheel));
}
