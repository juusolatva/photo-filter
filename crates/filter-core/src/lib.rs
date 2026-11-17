//! GUI-independent image storage and color math. All pixels are straight, linear Rec.2020.
pub mod buffer;
pub mod color;
pub mod edit;
pub mod histogram;
pub mod pipeline;
pub mod preset;
pub mod probe;
pub use buffer::ImageBuffer;

pub mod cpu_eval;
