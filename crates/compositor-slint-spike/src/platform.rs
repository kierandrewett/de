//! Custom Slint platform implementation driven by smithay's calloop event loop.
//!
//! DELIVERABLE 1: Implement slint::platform::Platform that:
//!   - Uses calloop's loop signal for wakeups
//!   - Tracks time via std::time::Instant (smithay's Clock<Monotonic> is for wayland timestamps)
//!   - Drives animation timers through calloop
//!   - Exposes a MinimalSoftwareWindow as the window adapter
//!
//! SPIKE: Minimal implementation — no clipboard, no URL opening.

use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use slint::platform::{
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    Platform, WindowAdapter,
};
use smithay::reexports::calloop::LoopSignal;
use tracing::{debug, trace};

/// Our custom Platform implementation.
/// Holds the calloop loop signal (for wakeup) and the start time (for animation timing).
pub struct CalloopPlatform {
    /// Signal to wake up the calloop event loop when Slint needs a re-render.
    loop_signal: LoopSignal,
    /// When the compositor started (monotonic).
    start_time: Instant,
    /// The single window adapter; created on first call to create_window_adapter.
    /// SPIKE: single-window assumption — fine for a compositor spike.
    pub window: Arc<Mutex<Option<Rc<MinimalSoftwareWindow>>>>,
}

impl CalloopPlatform {
    pub fn new(loop_signal: LoopSignal) -> Self {
        Self {
            loop_signal,
            start_time: Instant::now(),
            window: Arc::new(Mutex::new(None)),
        }
    }

    /// Returns a clone of the window handle if one has been created.
    pub fn window_handle(&self) -> Option<Rc<MinimalSoftwareWindow>> {
        self.window.lock().unwrap().clone()
    }
}

impl Platform for CalloopPlatform {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        debug!("CalloopPlatform::create_window_adapter called");

        // SPIKE: RepaintBufferType::NewBuffer means Slint always provides a
        // fresh full-frame buffer — simpler than partial repaints for the spike.
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);

        // Store for later retrieval by the render loop
        *self.window.lock().unwrap() = Some(window.clone());

        Ok(window)
    }

    fn duration_since_start(&self) -> Duration {
        self.start_time.elapsed()
    }

    fn run_event_loop(&self) -> Result<(), slint::PlatformError> {
        // SPIKE: We don't use Slint's event loop — calloop drives everything.
        // Returning an error here would break things, so we just return Ok.
        // The real event loop is in renderer::run().
        Err(slint::PlatformError::Other(
            "CalloopPlatform does not support run_event_loop; use calloop directly".into(),
        ))
    }
}
