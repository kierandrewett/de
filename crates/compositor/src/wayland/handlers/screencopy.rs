//! Screen capture handlers — ext-image-capture-source-v1 and ext-image-copy-capture-v1.

use smithay::{
    delegate_image_capture_source, delegate_image_copy_capture, delegate_output_capture_source,
    output::Output,
    wayland::image_capture_source::{
        ImageCaptureSource, ImageCaptureSourceHandler,
        OutputCaptureSourceHandler, OutputCaptureSourceState,
    },
    wayland::image_copy_capture::{
        BufferConstraints, Frame, FrameRef, ImageCopyCaptureHandler,
        ImageCopyCaptureState, Session, SessionRef,
    },
};

use crate::state::State;

// ─── ImageCaptureSource (base trait) ─────────────────────────────────────────

impl ImageCaptureSourceHandler for State {
    fn source_destroyed(&mut self, _source: ImageCaptureSource) {}
}

delegate_image_capture_source!(State);

// ─── OutputCaptureSource ──────────────────────────────────────────────────────

impl OutputCaptureSourceHandler for State {
    fn output_capture_source_state(&mut self) -> &mut OutputCaptureSourceState {
        &mut self.common.output_capture_source_state
    }

    fn output_source_created(&mut self, _source: ImageCaptureSource, _output: &Output) {}
}

delegate_output_capture_source!(State);

// ─── ImageCopyCapture ─────────────────────────────────────────────────────────

impl ImageCopyCaptureHandler for State {
    fn image_copy_capture_state(&mut self) -> &mut ImageCopyCaptureState {
        &mut self.common.image_copy_capture_state
    }

    fn capture_constraints(
        &mut self,
        _source: &ImageCaptureSource,
    ) -> Option<BufferConstraints> {
        // TODO: subagent 08 (render) provides real format/modifier constraints.
        // Return None for now to reject capture until rendering is wired.
        None
    }

    fn new_session(&mut self, _session: Session) {
        // Subagent 08 stores sessions for render-loop integration.
    }

    fn frame(&mut self, _session: &SessionRef, frame: Frame) {
        use smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_frame_v1::FailureReason;
        frame.fail(FailureReason::Stopped);
    }

    fn frame_aborted(&mut self, _frame: FrameRef) {}
}

delegate_image_copy_capture!(State);
