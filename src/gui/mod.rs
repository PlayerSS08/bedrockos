//! BedrockOS graphics layer. Everything here is pure (no hardware access) and unit-tested on the host:
//! the kernel side only hands it a pixel buffer and copies the result to the framebuffer.

pub mod canvas;
pub mod math;
pub mod theme;
pub mod wallimg;
