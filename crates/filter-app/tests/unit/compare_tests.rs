use super::Compare;
use eframe::egui::{Rect, pos2, vec2};

/// A canvas offset from the window origin, as beside the toolbar and panel.
fn viewport() -> Rect {
    Rect::from_min_size(pos2(10.25, 40.0), vec2(601.0, 400.0))
}

#[test]
fn off_shows_the_edit_everywhere() {
    let compare = Compare::default();
    assert_eq!(compare.divider_x(viewport(), 1.0), None);
    assert_eq!(compare.before_until(viewport(), 1.0), 0.0);
}

#[test]
fn holding_shows_the_original_everywhere_even_when_split() {
    for split in [None, Some(0.5)] {
        let compare = Compare { split, held: true };
        assert_eq!(compare.before_until(viewport(), 1.5), f32::MAX);
    }
}

#[test]
fn split_lands_on_a_physical_pixel_boundary_shared_with_the_divider() {
    for ppp in [1.0, 1.25, 1.5, 2.0] {
        for fraction in [0.0, 0.3, Compare::DEFAULT_SPLIT, 0.77, 1.0] {
            let compare = Compare {
                split: Some(fraction),
                held: false,
            };
            let x = compare.divider_x(viewport(), ppp).unwrap();
            let physical = compare.before_until(viewport(), ppp);
            assert_eq!(physical, physical.round(), "{ppp} {fraction}");
            assert!((x * ppp - physical).abs() < 1e-3, "{ppp} {fraction}");
            let ideal = viewport().min.x + fraction * viewport().width();
            assert!((x - ideal).abs() <= 0.5 / ppp + 1e-4, "{x} vs {ideal}");
        }
    }
}

#[test]
fn out_of_range_fractions_stay_inside_the_canvas() {
    for (fraction, edge) in [(-0.5, viewport().min.x), (1.5, viewport().max.x)] {
        let compare = Compare {
            split: Some(fraction),
            held: false,
        };
        let x = compare.divider_x(viewport(), 2.0).unwrap();
        assert!((x - edge).abs() <= 0.25, "{x} vs {edge}");
    }
}
