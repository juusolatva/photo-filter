use super::ClippingOverlay;

#[test]
fn toggle_turns_both_on_unless_both_are_already_on() {
    let on = ClippingOverlay {
        shadows: true,
        highlights: true,
    };
    for start in [
        ClippingOverlay::default(),
        ClippingOverlay {
            shadows: true,
            highlights: false,
        },
        ClippingOverlay {
            shadows: false,
            highlights: true,
        },
    ] {
        let mut overlay = start;
        overlay.toggle_all();
        assert_eq!(overlay, on, "{start:?}");
    }
    let mut overlay = on;
    overlay.toggle_all();
    assert_eq!(overlay, ClippingOverlay::default());
}
