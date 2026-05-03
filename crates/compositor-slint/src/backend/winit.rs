//! Nested winit backend.
//!
//! This preserves the compositor's existing development/runtime path while the
//! production udev/DRM backend is built out behind `backend::udev`.

use anyhow::Result;

pub fn run() -> Result<()> {
    crate::renderer::run()
}
