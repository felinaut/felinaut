//! Browser-facing page facade. The shared API is also used by native adapter harnesses.

#[path = "page/api.rs"]
mod api;
pub use api::*;

#[cfg(target_os = "macos")]
#[path = "page/macos.rs"]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{backend_info, create, create_hidden};
