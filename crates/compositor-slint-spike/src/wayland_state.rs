//! Minimal Wayland compositor state for the Slint spike.
//!
//! DELIVERABLE 3: Open a wayland socket, accept one client, import their SHM buffer.
//!
//! Protocols implemented (bare minimum to run kitty):
//!   - wl_compositor
//!   - wl_shm (SHM buffer import)
//!   - wl_seat (pointer + keyboard)
//!   - wl_output
//!   - xdg-shell (toplevels)
//!   - linux-dmabuf (advertise but log-only)
//!
//! SPIKE: No space/window management, no damage tracking, no animations.

use std::sync::{Arc, Mutex};

use smithay::{
    delegate_compositor, delegate_dmabuf, delegate_output, delegate_seat, delegate_shm,
    delegate_xdg_shell,
    backend::renderer::utils::on_commit_buffer_handler,
    input::{pointer::CursorImageStatus, Seat, SeatHandler, SeatState},
    reexports::{
        calloop::{LoopHandle, LoopSignal},
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer::WlBuffer, wl_surface::WlSurface},
            Client, DisplayHandle,
        },
    },
    utils::{Clock, Monotonic, SERIAL_COUNTER},
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState},
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        output::OutputManagerState,
        selection::{
            data_device::{
                DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler,
            },
            primary_selection::{PrimarySelectionHandler, PrimarySelectionState},
            SelectionHandler,
        },
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
        },
        shm::{ShmHandler, ShmState},
    },
};
use tracing::{debug, info, warn};

// ──────────────────────────────────────────────────────────────────────────────
// Client state
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {
        debug!("wayland client connected");
    }
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {
        info!("wayland client disconnected");
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Client pixel buffer shared between commit handler and render loop
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct ClientSurfaceData {
    pub pixels: Vec<u8>, // RGBA8 premultiplied
    pub width: u32,
    pub height: u32,
    pub dirty: bool,
}

// ──────────────────────────────────────────────────────────────────────────────
// Compositor state
// ──────────────────────────────────────────────────────────────────────────────

pub struct SpikeState {
    pub display_handle: DisplayHandle,
    pub loop_handle: LoopHandle<'static, SpikeState>,
    pub loop_signal: LoopSignal,
    pub clock: Clock<Monotonic>,

    // Protocol state
    pub compositor_state: CompositorState,
    pub shm_state: ShmState,
    pub seat_state: SeatState<Self>,
    pub seat: Seat<Self>,
    pub xdg_shell_state: XdgShellState,
    pub output_manager_state: OutputManagerState,
    pub dmabuf_state: DmabufState,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,

    // Active toplevel
    pub active_surface: Option<WlSurface>,

    /// Shared pixel buffer
    pub client_pixels: Arc<Mutex<ClientSurfaceData>>,

    pub should_exit: bool,
    pub pointer_pos: (f64, f64),
}

impl SpikeState {
    pub fn new(
        display_handle: DisplayHandle,
        loop_handle: LoopHandle<'static, SpikeState>,
        loop_signal: LoopSignal,
    ) -> Self {
        let clock = Clock::<Monotonic>::new();
        let compositor_state = CompositorState::new::<Self>(&display_handle);
        let shm_state = ShmState::new::<Self>(&display_handle, vec![]);
        let mut seat_state = SeatState::new();
        let seat = seat_state.new_wl_seat(&display_handle, "seat-spike");
        let xdg_shell_state = XdgShellState::new::<Self>(&display_handle);
        let output_manager_state =
            OutputManagerState::new_with_xdg_output::<Self>(&display_handle);
        let dmabuf_state = DmabufState::new();
        let data_device_state = DataDeviceState::new::<Self>(&display_handle);
        let primary_selection_state = PrimarySelectionState::new::<Self>(&display_handle);

        let mut state = Self {
            display_handle,
            loop_handle,
            loop_signal,
            clock,
            compositor_state,
            shm_state,
            seat_state,
            seat,
            xdg_shell_state,
            output_manager_state,
            dmabuf_state,
            data_device_state,
            primary_selection_state,
            active_surface: None,
            client_pixels: Arc::new(Mutex::new(ClientSurfaceData::default())),
            should_exit: false,
            pointer_pos: (0.0, 0.0),
        };

        state.seat.add_keyboard(Default::default(), 200, 25).ok();
        state.seat.add_pointer();

        state
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// CompositorHandler
// ──────────────────────────────────────────────────────────────────────────────

impl CompositorHandler for SpikeState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);

        let is_active = self
            .active_surface
            .as_ref()
            .map(|s| s == surface)
            .unwrap_or(false);

        if is_active {
            import_shm_buffer(surface, &self.client_pixels);
        }
    }
}

delegate_compositor!(SpikeState);

fn import_shm_buffer(surface: &WlSurface, pixels_out: &Arc<Mutex<ClientSurfaceData>>) {
    use smithay::backend::renderer::utils::with_renderer_surface_state;
    use smithay::wayland::shm::with_buffer_contents;

    // with_renderer_surface_state returns Option<T>
    let buffer = match with_renderer_surface_state(surface, |s| s.buffer().cloned()) {
        Some(Some(b)) => b,
        _ => return,
    };

    // with_buffer_contents closure signature: |ptr: *const u8, len: usize, spec: BufferData| -> T
    let result = with_buffer_contents(&*buffer, |ptr: *const u8, len: usize, spec| {
        let width = spec.width as u32;
        let height = spec.height as u32;
        let stride = spec.stride as usize;

        // Safety: smithay guarantees the pointer is valid for the duration of this closure
        let data = unsafe { std::slice::from_raw_parts(ptr, len) };

        let mut rgba = vec![0u8; (width * height * 4) as usize];

        for y in 0..height as usize {
            for x in 0..width as usize {
                let src = y * stride + x * 4;
                let dst = (y * width as usize + x) * 4;
                if src + 4 > data.len() {
                    break;
                }
                // wl_shm ARGB8888 in LE memory: [B, G, R, A]
                let b = data[src];
                let g = data[src + 1];
                let r = data[src + 2];
                let a = data[src + 3];
                // Premultiply alpha for Slint
                let af = a as f32 / 255.0;
                rgba[dst]     = (r as f32 * af) as u8;
                rgba[dst + 1] = (g as f32 * af) as u8;
                rgba[dst + 2] = (b as f32 * af) as u8;
                rgba[dst + 3] = a;
            }
        }

        info!(
            "client surface imported, {}x{} SHM buffer",
            width, height
        );

        ClientSurfaceData {
            pixels: rgba,
            width,
            height,
            dirty: true,
        }
    });

    match result {
        Ok(data) => *pixels_out.lock().unwrap() = data,
        Err(e) => warn!("SHM read failed: {:?}", e),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// BufferHandler + ShmHandler
// ──────────────────────────────────────────────────────────────────────────────

impl BufferHandler for SpikeState {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for SpikeState {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

delegate_shm!(SpikeState);

// ──────────────────────────────────────────────────────────────────────────────
// DmabufHandler
// ──────────────────────────────────────────────────────────────────────────────

impl DmabufHandler for SpikeState {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: smithay::backend::allocator::dmabuf::Dmabuf,
        notifier: ImportNotifier,
    ) {
        warn!(
            planes = dmabuf.num_planes(),
            "DMA-BUF import requested — not supported in spike (client should use SHM)"
        );
        // Dropping notifier signals failure to the client
        drop(notifier);
    }
}

delegate_dmabuf!(SpikeState);

// ──────────────────────────────────────────────────────────────────────────────
// SeatHandler
// ──────────────────────────────────────────────────────────────────────────────

impl SeatHandler for SpikeState {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(
        &mut self,
        _seat: &Seat<Self>,
        _focused: Option<&Self::KeyboardFocus>,
    ) {
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: CursorImageStatus) {}
}

delegate_seat!(SpikeState);

// ──────────────────────────────────────────────────────────────────────────────
// XdgShellHandler
// ──────────────────────────────────────────────────────────────────────────────

impl XdgShellHandler for SpikeState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        info!("new xdg toplevel");
        surface.with_pending_state(|s| {
            s.size = Some((800, 600).into());
        });
        surface.send_configure();

        self.active_surface = Some(surface.wl_surface().clone());

        if let Some(kb) = self.seat.get_keyboard() {
            kb.set_focus(
                self,
                Some(surface.wl_surface().clone()),
                SERIAL_COUNTER.next_serial(),
            );
        }
    }

    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}

    fn toplevel_destroyed(&mut self, _surface: ToplevelSurface) {
        info!("toplevel destroyed");
        self.active_surface = None;
        *self.client_pixels.lock().unwrap() = ClientSurfaceData::default();
    }

    fn popup_destroyed(&mut self, _surface: PopupSurface) {}

    fn grab(
        &mut self,
        _surface: PopupSurface,
        _seat: smithay::reexports::wayland_server::protocol::wl_seat::WlSeat,
        _serial: smithay::utils::Serial,
    ) {
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|s| {
            s.geometry = positioner.get_geometry();
        });
        surface.send_repositioned(token);
    }
}

delegate_xdg_shell!(SpikeState);

// ──────────────────────────────────────────────────────────────────────────────
// OutputHandler
// ──────────────────────────────────────────────────────────────────────────────

impl smithay::wayland::output::OutputHandler for SpikeState {}

delegate_output!(SpikeState);

// ──────────────────────────────────────────────────────────────────────────────
// SelectionHandler (required by DataDeviceHandler + PrimarySelectionHandler)
// ──────────────────────────────────────────────────────────────────────────────

impl SelectionHandler for SpikeState {
    type SelectionUserData = ();
}

// ──────────────────────────────────────────────────────────────────────────────
// DataDeviceHandler
// ──────────────────────────────────────────────────────────────────────────────

impl WaylandDndGrabHandler for SpikeState {}

impl DataDeviceHandler for SpikeState {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

smithay::delegate_data_device!(SpikeState);

// ──────────────────────────────────────────────────────────────────────────────
// PrimarySelectionHandler
// ──────────────────────────────────────────────────────────────────────────────

impl PrimarySelectionHandler for SpikeState {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.primary_selection_state
    }
}

smithay::delegate_primary_selection!(SpikeState);
