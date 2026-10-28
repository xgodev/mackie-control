//! The PreSonus Quantum HD 8 over UCNet: the part of jpfaria/quantum-hd8 the bridge uses.

pub mod client;
pub mod ucnet;

pub use client::{Client, Error, SceneLoad};
