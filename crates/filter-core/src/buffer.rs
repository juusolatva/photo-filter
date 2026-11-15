use half::f16;

/// Immutable source storage; alpha is straight, RGB is linear Rec.2020/D65.
#[derive(Clone, Debug)]
pub struct ImageBuffer {
    width: u32,
    height: u32,
    pixels: Vec<[f16; 4]>,
}
impl ImageBuffer {
    pub fn new(width: u32, height: u32, pixels: Vec<[f16; 4]>) -> Result<Self, &'static str> {
        let count = (width as usize)
            .checked_mul(height as usize)
            .ok_or("image dimensions overflow")?;
        if width == 0 || height == 0 || pixels.len() != count {
            return Err("invalid image dimensions or pixel count");
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn pixels(&self) -> &[[f16; 4]] {
        &self.pixels
    }

    /// Exact area reduction in linear light, weighted by alpha to avoid dark fringes.
    pub fn proxy(&self, max_edge: u32) -> Self {
        let max_edge = max_edge.max(1);
        let scale = (max_edge as f64 / self.width.max(self.height) as f64).min(1.0);
        if scale == 1.0 {
            return self.clone();
        }
        let w = (self.width as f64 * scale).round().max(1.0) as u32;
        let h = (self.height as f64 * scale).round().max(1.0) as u32;
        let sx = self.width as f64 / w as f64;
        let sy = self.height as f64 / h as f64;
        let mut pixels = Vec::with_capacity(w as usize * h as usize);
        for y in 0..h {
            for x in 0..w {
                let (x0, x1, y0, y1) = (
                    x as f64 * sx,
                    (x + 1) as f64 * sx,
                    y as f64 * sy,
                    (y + 1) as f64 * sy,
                );
                let mut sum = [0.0_f64; 4];
                for iy in y0.floor() as u32..(y1.ceil() as u32).min(self.height) {
                    for ix in x0.floor() as u32..(x1.ceil() as u32).min(self.width) {
                        let weight = (x1.min((ix + 1) as f64) - x0.max(ix as f64))
                            * (y1.min((iy + 1) as f64) - y0.max(iy as f64));
                        let p = self.pixels[(iy * self.width + ix) as usize].map(f16::to_f32);
                        for c in 0..3 {
                            sum[c] += p[c] as f64 * p[3] as f64 * weight;
                        }
                        sum[3] += p[3] as f64 * weight;
                    }
                }
                let alpha = sum[3];
                pixels.push([
                    f16::from_f64(if alpha > 0.0 { sum[0] / alpha } else { 0.0 }),
                    f16::from_f64(if alpha > 0.0 { sum[1] / alpha } else { 0.0 }),
                    f16::from_f64(if alpha > 0.0 { sum[2] / alpha } else { 0.0 }),
                    f16::from_f64(alpha / (sx * sy)),
                ]);
            }
        }
        Self {
            width: w,
            height: h,
            pixels,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_shape() {
        assert!(ImageBuffer::new(0, 1, vec![]).is_err());
        assert!(ImageBuffer::new(2, 1, vec![[f16::ZERO; 4]]).is_err());
    }
    #[test]
    fn proxy_is_linear_and_alpha_weighted() {
        let image = ImageBuffer::new(2, 1, vec![[f16::ONE; 4], [f16::ZERO; 4]]).unwrap();
        let p = image.proxy(1).pixels[0].map(f16::to_f32);
        assert_eq!(p, [1.0, 1.0, 1.0, 0.5]);
    }
    #[test]
    fn proxy_retains_thin_images() {
        let image = ImageBuffer::new(1, 100, vec![[f16::ONE; 4]; 100]).unwrap();
        assert_eq!((image.proxy(2).width(), image.proxy(2).height()), (1, 2));
    }
}
