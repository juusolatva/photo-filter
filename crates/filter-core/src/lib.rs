//! GUI-independent image storage and color math. All pixels are straight, linear Rec.2020.
pub mod buffer;
pub mod color;
pub mod curve;
pub mod edit;
pub mod grading;
pub mod histogram;
pub mod lut;
pub mod pipeline;
pub mod preset;
pub mod probe;
pub mod white_balance;
pub use buffer::ImageBuffer;

pub mod cpu_eval;
