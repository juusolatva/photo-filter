use super::*;
use filter_core::{
    edit::EditState,
    lut::{CubeLut, LutSlot},
};
use std::sync::Arc;

fn lut(offset: f32) -> Arc<CubeLut> {
    let texels = (0..3 * 8)
        .map(|i| half::f16::from_f32(offset + (i % 2) as f32 * 0.5).to_bits())
        .collect();
    Arc::new(CubeLut::new(2, "test", [0.0; 3], [1.0; 3], texels).unwrap())
}
fn adjustments(lut: &Arc<CubeLut>, strength: f32) -> BasicAdjustments {
    let mut edit = EditState::default();
    edit.finishing.lut = LutSlot::Cube(lut.clone());
    edit.finishing.lut_strength = strength;
    BasicAdjustments::new(&edit).unwrap()
}

#[test]
fn a_lut_only_edit_changes_the_snapshot_although_the_rows_do_not() {
    let none = BasicAdjustments::default();
    let first = lut(0.1);
    let active = adjustments(&first, 1.0);
    assert_eq!(none.uniform_rows(), active.uniform_rows());
    assert_ne!(RenderSnapshot::new(&none), RenderSnapshot::new(&active));
    // Same LUT and strength compare equal across rebuilt snapshots.
    assert_eq!(
        RenderSnapshot::new(&active),
        RenderSnapshot::new(&adjustments(&first, 1.0))
    );
    // Strength and a different lattice both count as changes; strength 0 is no LUT.
    assert_ne!(
        RenderSnapshot::new(&active),
        RenderSnapshot::new(&adjustments(&first, 0.5))
    );
    assert_ne!(
        RenderSnapshot::new(&active),
        RenderSnapshot::new(&adjustments(&lut(0.2), 1.0))
    );
    assert_eq!(
        RenderSnapshot::new(&none),
        RenderSnapshot::new(&adjustments(&first, 0.0))
    );
}

fn with_master(points: &[[f32; 2]]) -> BasicAdjustments {
    let mut edit = EditState::default();
    edit.curves.master = filter_core::edit::Curve(points.to_vec());
    BasicAdjustments::new(&edit).unwrap()
}

#[test]
fn a_curve_point_drag_changes_the_snapshot_although_the_rows_do_not() {
    let a = with_master(&[[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]]);
    let b = with_master(&[[0.0, 0.0], [0.5, 0.4], [1.0, 1.0]]);
    assert_eq!(a.uniform_rows(), b.uniform_rows());
    assert_ne!(RenderSnapshot::new(&a), RenderSnapshot::new(&b));
    // A rebuild bakes a fresh Arc with the same content and compares equal.
    let again = with_master(&[[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]]);
    assert!(!Arc::ptr_eq(a.curves().unwrap(), again.curves().unwrap()));
    assert_eq!(RenderSnapshot::new(&a), RenderSnapshot::new(&again));
}
