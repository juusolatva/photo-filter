//! GUI-independent image storage and color math. All pixels are straight, linear Rec.2020.
pub mod buffer;
pub mod color;
pub mod edit;
pub mod pipeline;
pub mod preset;
pub use buffer::ImageBuffer;
