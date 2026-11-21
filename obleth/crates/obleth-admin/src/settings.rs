//! Runtime settings handlers, one module per settings page.

use super::*;

mod alerts;
mod auto_router;
mod boons;
mod energy;
mod misc;

pub use alerts::*;
pub use auto_router::*;
pub use boons::*;
pub use energy::*;
pub use misc::*;
