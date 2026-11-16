use super::{ImageBuffer, f16};
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
