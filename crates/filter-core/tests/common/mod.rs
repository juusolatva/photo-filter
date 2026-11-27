#![allow(dead_code)]
use filter_core::{
    edit::EditState,
    lut::{CubeLut, LutSlot, parse_cube},
};
use std::sync::Arc;

pub fn cube_text(n: usize, header: &str, f: impl Fn(usize, usize, usize) -> [f32; 3]) -> String {
    let mut out = format!("{header}\nLUT_3D_SIZE {n}\n");
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let [x, y, z] = f(r, g, b);
                out.push_str(&format!("{x} {y} {z}\n"));
            }
        }
    }
    out
}
pub fn cube(n: usize, f: impl Fn(usize, usize, usize) -> [f32; 3]) -> CubeLut {
    parse_cube(cube_text(n, "", f).as_bytes()).unwrap()
}
pub fn identity(n: usize) -> CubeLut {
    let s = (n - 1) as f32;
    cube(n, |r, g, b| [r as f32 / s, g as f32 / s, b as f32 / s])
}
/// N=2 lattice mapping every encoded channel `e` in [0, 1] to `1 - e`; exact in f16.
pub fn invert() -> CubeLut {
    cube(2, |r, g, b| {
        [1.0 - r as f32, 1.0 - g as f32, 1.0 - b as f32]
    })
}
pub fn with_lut(lut: CubeLut, strength: f32) -> EditState {
    let mut edit = EditState::default();
    edit.finishing.lut = LutSlot::Cube(Arc::new(lut));
    edit.finishing.lut_strength = strength;
    edit
}
