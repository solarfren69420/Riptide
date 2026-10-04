//! Direct3D 9 shader bytecode (shader model 2 and 3), with no ties to any one game:
//! - [`disasm`]: find the programs in a blob (an effect file, an executable, a game archive),
//!   read their constant tables and disassemble them with the constants' names;
//! - [`wgsl`]: translate a program to WGSL, so the original shaders run on wgpu / Bevy.

pub mod disasm;
pub mod wgsl;

pub use disasm::{disassemble, programs, Constant, Program};
