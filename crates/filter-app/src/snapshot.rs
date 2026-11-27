//! Change detection for GPU measurements (histogram, probe) of an adjustment snapshot.
use filter_core::{
    cpu_eval::{BasicAdjustments, BasicRows, LutStage},
    curve::CurveLut,
};

/// The uniform rows plus the baked tone curves and the active LUT. A LUT-only
/// edit, or a curve-point drag once curves are active, leaves the rows unchanged,
/// so rows alone must not decide whether a measurement is current.
/// Curves compare by content, since every `BasicAdjustments` rebuild bakes a new
/// `Arc`. LUTs compare by `Arc` identity and strength: a rebuilt snapshot of the
/// same edit shares the LUT, and a different `Arc` only costs one redundant measurement.
#[derive(Clone, Debug)]
pub struct RenderSnapshot {
    rows: BasicRows,
    curves: Option<std::sync::Arc<CurveLut>>,
    lut: Option<LutStage>,
}
impl RenderSnapshot {
    pub fn new(adjustments: &BasicAdjustments) -> Self {
        Self {
            rows: *adjustments.uniform_rows(),
            curves: adjustments.curves().cloned(),
            lut: adjustments.lut().cloned(),
        }
    }
}
impl PartialEq for RenderSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.rows == other.rows
            && self.curves == other.curves // `Arc<T>` equality is by content.
            && match (&self.lut, &other.lut) {
                (None, None) => true,
                (Some(a), Some(b)) => {
                    std::sync::Arc::ptr_eq(&a.lut, &b.lut) && a.strength == b.strength
                }
                _ => false,
            }
    }
}

#[cfg(test)]
#[path = "../tests/unit/snapshot_tests.rs"]
mod tests;
