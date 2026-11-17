use super::{Placement, ViewInput, ViewState};
use eframe::egui::{Rect, Vec2, pos2, vec2};

/// 600 × 400 point canvas offset from the window origin.
fn viewport() -> Rect {
    Rect::from_min_size(pos2(40.0, 60.0), vec2(600.0, 400.0))
}
/// Fits at 0.5 points per source pixel: 600 × 300 points, letterboxed.
const SOURCE: Vec2 = vec2(1200.0, 600.0);

fn place(view: &mut ViewState, ppp: f32, input: ViewInput) -> Placement {
    view.place(viewport(), SOURCE, ppp, input).unwrap()
}
fn assert_aligned(rect: Rect, ppp: f32) {
    for v in [rect.min.x, rect.min.y, rect.max.x, rect.max.y] {
        let physical = v * ppp;
        assert!(
            (physical - physical.round()).abs() < 1e-3,
            "{rect:?} is not on the {ppp}x physical pixel grid"
        );
    }
}

#[test]
fn fit_centers_the_whole_image_and_resets_the_view() {
    let mut view = ViewState::default();
    let p = place(&mut view, 2.0, ViewInput::default());
    assert_eq!(
        p.image,
        Rect::from_center_size(viewport().center(), vec2(600.0, 300.0))
    );
    assert_eq!(p.visible, p.image);
    assert_eq!(p.physical_scale, 1.0); // 0.5 points per pixel at 2x DPI.

    view = ViewState {
        zoom: 3.0,
        native_pixels: true,
        pan: vec2(30.0, -4.0),
    };
    view.fit();
    assert_eq!(view, ViewState::default());
}

#[test]
fn native_zoom_maps_source_pixels_to_physical_pixels_through_pan_resize_and_dpi() {
    for ppp in [1.0, 1.25, 1.5, 2.0] {
        let mut view = ViewState {
            pan: vec2(13.3, -7.1),
            ..ViewState::default()
        };
        let native = ViewInput {
            native_zoom: true,
            ..ViewInput::default()
        };
        let p = place(&mut view, ppp, native);
        assert!(view.native_pixels);
        assert_eq!(view.pan, Vec2::ZERO, "100% recenters the image");
        assert_eq!(p.physical_scale, 1.0);
        assert!((p.image.width() * ppp - SOURCE.x).abs() < 1e-2);
        assert_aligned(p.image, ppp);
        assert_aligned(p.visible, ppp);

        // Fractional drags keep 100% mode on the physical pixel grid.
        let drag = ViewInput {
            drag: Some(vec2(0.37, 0.61)),
            ..ViewInput::default()
        };
        let p = place(&mut view, ppp, drag);
        assert!(view.native_pixels);
        assert_eq!(p.physical_scale, 1.0);
        assert_aligned(p.image, ppp);
        assert_aligned(p.visible, ppp);
        assert!(viewport().contains_rect(p.visible));

        // A resized canvas recomputes zoom so 100% stays one-to-one.
        let small = Rect::from_min_size(pos2(40.0, 60.0), vec2(300.0, 250.0));
        let p = view
            .place(small, SOURCE, ppp, ViewInput::default())
            .unwrap();
        assert!((p.image.width() * ppp - SOURCE.x).abs() < 1e-2);
        assert_aligned(p.visible, ppp);
    }
}

#[test]
fn scroll_zoom_keeps_the_image_point_under_the_pointer() {
    let mut view = ViewState::default();
    let pointer = pos2(200.0, 150.0);
    let before = place(&mut view, 1.0, ViewInput::default());
    let anchor = (pointer - before.image.min) / before.image.size();
    for scroll in [120.0, 340.0, -90.0] {
        let after = place(
            &mut view,
            1.0,
            ViewInput {
                scroll,
                pointer: Some(pointer),
                ..ViewInput::default()
            },
        );
        let moved = (pointer - after.image.min) / after.image.size();
        assert!((anchor - moved).length() < 1e-4, "{anchor:?} != {moved:?}");
    }
    assert!(view.zoom > 1.0);
}

#[test]
fn scrolling_leaves_native_mode() {
    let mut view = ViewState::default();
    place(
        &mut view,
        1.5,
        ViewInput {
            native_zoom: true,
            ..ViewInput::default()
        },
    );
    let p = place(
        &mut view,
        1.5,
        ViewInput {
            scroll: 50.0,
            ..ViewInput::default()
        },
    );
    assert!(!view.native_pixels);
    assert!(p.physical_scale > 1.0);
}

#[test]
fn scroll_zoom_is_limited_to_1_and_3200_percent_but_always_reaches_fit() {
    let zoom_in = ViewInput {
        scroll: 1000.0,
        ..ViewInput::default()
    };
    let zoom_out = ViewInput {
        scroll: -1000.0,
        ..ViewInput::default()
    };
    let mut view = ViewState::default();
    let mut p = place(&mut view, 1.0, zoom_in);
    for _ in 0..50 {
        p = place(&mut view, 1.0, zoom_in);
    }
    assert!((p.physical_scale - 32.0).abs() < 1e-3);
    for _ in 0..50 {
        p = place(&mut view, 1.0, zoom_out);
    }
    assert!((p.physical_scale - 0.01).abs() < 1e-6);

    // A tiny image already exceeds 3200% at Fit; it can return to, but not
    // beyond, Fit. Zooming out stays possible.
    let tiny = vec2(10.0, 5.0);
    let mut view = ViewState {
        zoom: 0.5,
        ..ViewState::default()
    };
    let fit = 60.0;
    for _ in 0..50 {
        p = view.place(viewport(), tiny, 1.0, zoom_in).unwrap();
    }
    assert_eq!(view.zoom, 1.0);
    assert!((p.physical_scale - fit).abs() < 1e-3);
}

#[test]
fn drag_pans_and_visible_area_is_clipped_to_the_viewport() {
    let mut view = ViewState {
        zoom: 4.0,
        ..ViewState::default()
    };
    let p = place(
        &mut view,
        1.0,
        ViewInput {
            drag: Some(vec2(25.0, -10.0)),
            ..ViewInput::default()
        },
    );
    assert_eq!(view.pan, vec2(25.0, -10.0));
    assert_eq!(p.image.center(), viewport().center() + vec2(25.0, -10.0));
    assert_eq!(p.visible, viewport(), "a 4x image covers the whole canvas");
}

#[test]
fn empty_or_non_finite_viewports_place_nothing_and_ignore_input() {
    let mut view = ViewState::default();
    let empty = Rect::from_min_size(pos2(0.0, 0.0), vec2(0.0, 400.0));
    let input = ViewInput {
        native_zoom: true,
        drag: Some(vec2(5.0, 5.0)),
        scroll: 50.0,
        pointer: Some(pos2(1.0, 1.0)),
    };
    assert!(view.place(empty, SOURCE, 1.0, input).is_none());
    let nan = Rect::from_min_size(pos2(0.0, 0.0), vec2(f32::NAN, 400.0));
    assert!(view.place(nan, SOURCE, 1.0, input).is_none());
    let unbounded = Rect::from_min_size(pos2(0.0, 0.0), vec2(f32::INFINITY, f32::INFINITY));
    assert!(view.place(unbounded, SOURCE, 1.0, input).is_none());
    let canvas = viewport();
    for source in [vec2(f32::NAN, 600.0), vec2(0.0, 600.0), vec2(-1.0, -1.0)] {
        assert!(
            view.place(canvas, source, 1.0, input).is_none(),
            "{source:?}"
        );
    }
    for ppp in [0.0, f32::NAN, f32::INFINITY] {
        assert!(view.place(canvas, SOURCE, ppp, input).is_none(), "{ppp}");
    }
    assert_eq!(view, ViewState::default());
}
