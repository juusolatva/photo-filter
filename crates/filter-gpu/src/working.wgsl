// Pointwise working-space operation shared by the viewport and full-resolution tiles.
// Straight linear Rec.2020: retain negative values, HDR headroom and alpha.
fn apply_exposure(pixel: vec4<f32>, ev: f32) -> vec4<f32> {
    return vec4(pixel.rgb * exp2(ev), pixel.a);
}
