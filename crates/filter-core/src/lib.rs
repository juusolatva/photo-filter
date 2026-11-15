//! GUI-independent image storage and color math. All pixels are straight, linear Rec.2020.
pub mod buffer;
pub mod color;
pub use buffer::ImageBuffer;
