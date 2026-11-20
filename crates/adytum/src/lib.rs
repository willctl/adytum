#![doc = include_str!("../../../README.md")]

mod cells;
mod driver;
mod error;
pub mod exec;
mod model;
pub mod policy;

#[cfg(feature = "doctor")]
pub mod doctor;
#[cfg(feature = "lima")]
pub mod lima;
#[cfg(feature = "openshell")]
pub mod openshell;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use cells::{home, Cells, Report};
pub use driver::*;
pub use error::{Error, Result};
pub use model::*;
