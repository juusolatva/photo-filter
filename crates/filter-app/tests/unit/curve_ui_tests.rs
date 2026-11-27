use super::{GAP, MAX_POINTS, insert, move_point, nearest, remove};
use filter_core::edit::{Curve, EditState};

fn three() -> Curve {
    Curve(vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]])
}
fn valid(curve: &Curve) -> bool {
    let mut edit = EditState::default();
    edit.curves.master = curve.clone();
    edit.validate().is_ok()
}

#[test]
fn nearest_picks_the_closest_point_within_the_radius() {
    let curve = three();
    assert_eq!(nearest(&curve, [0.52, 0.58], [0.05, 0.05]), Some(1));
    assert_eq!(nearest(&curve, [0.02, 0.01], [0.05, 0.05]), Some(0));
    assert_eq!(nearest(&curve, [0.3, 0.3], [0.05, 0.05]), None);
    // The radius is per axis: wide in x, narrow in y.
    assert_eq!(nearest(&curve, [0.58, 0.6], [0.1, 0.01]), Some(1));
    assert_eq!(nearest(&curve, [0.5, 0.63], [0.1, 0.01]), None);
}

#[test]
fn insertion_keeps_order_spacing_and_range() {
    let mut curve = three();
    assert_eq!(insert(&mut curve, [0.25, 1.4]), Some(1));
    assert_eq!(curve.0[1], [0.25, 1.0], "y is clamped into the plot");
    assert_eq!(insert(&mut curve, [0.75, -0.2]), Some(3));
    assert!(valid(&curve));
    // Too close to an existing point or to an endpoint.
    for x in [
        0.25 + GAP / 2.0,
        0.5 - GAP / 2.0,
        GAP / 2.0,
        1.0 - GAP / 2.0,
        0.0,
        1.0,
    ] {
        assert_eq!(insert(&mut curve, [x, 0.5]), None, "{x}");
    }
    assert_eq!(curve.0.len(), 5);
    // The model's point limit is respected.
    let mut full = Curve(
        (0..MAX_POINTS)
            .map(|i| {
                let x = i as f32 / (MAX_POINTS - 1) as f32;
                [x, x]
            })
            .collect(),
    );
    assert_eq!(insert(&mut full, [0.5005, 0.5]), None);
}

#[test]
fn moving_keeps_endpoint_x_neighbor_spacing_and_range() {
    let mut curve = Curve(vec![[0.0, 0.0], [0.3, 0.3], [0.6, 0.6], [1.0, 1.0]]);
    move_point(&mut curve, 0, [0.4, 0.2]);
    assert_eq!(curve.0[0], [0.0, 0.2], "endpoints only move vertically");
    move_point(&mut curve, 3, [0.5, 1.5]);
    assert_eq!(curve.0[3], [1.0, 1.0]);
    move_point(&mut curve, 1, [0.9, 0.5]);
    assert_eq!(
        curve.0[1],
        [0.6 - GAP, 0.5],
        "stops before its right neighbor"
    );
    move_point(&mut curve, 2, [0.0, -1.0]);
    assert_eq!(
        curve.0[2],
        [curve.0[1][0] + GAP, 0.0],
        "stops after its left neighbor"
    );
    assert!(valid(&curve));
    // Imported points closer than GAP keep their x.
    let mut tight = Curve(vec![
        [0.0, 0.0],
        [0.5, 0.5],
        [0.505, 0.55],
        [0.509, 0.6],
        [1.0, 1.0],
    ]);
    move_point(&mut tight, 2, [0.2, 0.4]);
    assert_eq!(tight.0[2], [0.505, 0.4]);
    assert!(valid(&tight));
}

#[test]
fn only_interior_points_are_removed() {
    let mut curve = three();
    assert!(!remove(&mut curve, 0));
    assert!(!remove(&mut curve, 2));
    assert!(remove(&mut curve, 1));
    assert_eq!(curve, Curve::default());
    assert!(!remove(&mut curve, 1));
}
