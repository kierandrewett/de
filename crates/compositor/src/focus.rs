//! Focus target types for keyboard, pointer, and touch input dispatch.

use std::{borrow::Cow, sync::Arc};

use smithay::{
    desktop::{LayerSurface, PopupKind, Window, WindowSurface},
    input::{
        dnd::{DndFocus, Source},
        keyboard::KeyboardTarget,
        pointer::{
            AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent,
            GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent,
            GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent,
            MotionEvent, PointerTarget, RelativeMotionEvent,
        },
        touch::TouchTarget,
        Seat,
    },
    reexports::wayland_server::{backend::ObjectId, protocol::wl_surface::WlSurface, DisplayHandle, Resource},
    utils::{IsAlive, Logical, Point, Serial},
    wayland::{
        seat::WaylandFocus,
        selection::data_device::WlOfferData,
    },
};

use crate::state::State;

/// Focus target for keyboard events — windows, layer surfaces, and popups.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyboardFocusTarget {
    Window(Box<Window>),
    LayerSurface(LayerSurface),
    Popup(Box<PopupKind>),
}

impl KeyboardFocusTarget {
    fn as_keyboard_target(&self) -> &dyn KeyboardTarget<State> {
        match self {
            Self::Window(w) => match w.underlying_surface() {
                WindowSurface::Wayland(t) => t.wl_surface(),
                WindowSurface::X11(s) => s,
            },
            Self::LayerSurface(l) => l.wl_surface(),
            Self::Popup(p) => p.wl_surface(),
        }
    }
}

impl IsAlive for KeyboardFocusTarget {
    fn alive(&self) -> bool {
        match self {
            Self::Window(w) => w.alive(),
            Self::LayerSurface(l) => l.alive(),
            Self::Popup(p) => p.alive(),
        }
    }
}

impl WaylandFocus for KeyboardFocusTarget {
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        match self {
            Self::Window(w) => w.wl_surface(),
            Self::LayerSurface(l) => Some(Cow::Borrowed(l.wl_surface())),
            Self::Popup(p) => Some(Cow::Borrowed(p.wl_surface())),
        }
    }

    fn same_client_as(&self, object_id: &ObjectId) -> bool {
        match self {
            Self::Window(w) => w.same_client_as(object_id),
            Self::LayerSurface(l) => l.wl_surface().id().same_client_as(object_id),
            Self::Popup(p) => p.wl_surface().id().same_client_as(object_id),
        }
    }
}

impl KeyboardTarget<State> for KeyboardFocusTarget {
    fn enter(
        &self,
        seat: &Seat<State>,
        data: &mut State,
        keys: Vec<smithay::input::keyboard::KeysymHandle<'_>>,
        serial: smithay::utils::Serial,
    ) {
        self.as_keyboard_target().enter(seat, data, keys, serial);
    }

    fn leave(&self, seat: &Seat<State>, data: &mut State, serial: smithay::utils::Serial) {
        self.as_keyboard_target().leave(seat, data, serial);
    }

    fn key(
        &self,
        seat: &Seat<State>,
        data: &mut State,
        key: smithay::input::keyboard::KeysymHandle<'_>,
        state: smithay::backend::input::KeyState,
        serial: smithay::utils::Serial,
        time: u32,
    ) {
        self.as_keyboard_target().key(seat, data, key, state, serial, time);
    }

    fn modifiers(
        &self,
        seat: &Seat<State>,
        data: &mut State,
        modifiers: smithay::input::keyboard::ModifiersState,
        serial: smithay::utils::Serial,
    ) {
        self.as_keyboard_target().modifiers(seat, data, modifiers, serial);
    }
}

/// Focus target for pointer events — windows and layer surfaces.
#[derive(Debug, Clone, PartialEq)]
pub enum PointerFocusTarget {
    Window(Box<Window>),
    LayerSurface(LayerSurface),
}

impl PointerFocusTarget {
    fn as_pointer_target(&self) -> &dyn PointerTarget<State> {
        match self {
            Self::Window(w) => match w.underlying_surface() {
                WindowSurface::Wayland(t) => t.wl_surface(),
                WindowSurface::X11(s) => s,
            },
            Self::LayerSurface(l) => l.wl_surface(),
        }
    }

    fn as_touch_target(&self) -> &dyn TouchTarget<State> {
        match self {
            Self::Window(w) => match w.underlying_surface() {
                WindowSurface::Wayland(t) => t.wl_surface(),
                WindowSurface::X11(s) => s,
            },
            Self::LayerSurface(l) => l.wl_surface(),
        }
    }
}

impl IsAlive for PointerFocusTarget {
    fn alive(&self) -> bool {
        match self {
            Self::Window(w) => w.alive(),
            Self::LayerSurface(l) => l.alive(),
        }
    }
}

impl WaylandFocus for PointerFocusTarget {
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        match self {
            Self::Window(w) => w.wl_surface(),
            Self::LayerSurface(l) => Some(Cow::Borrowed(l.wl_surface())),
        }
    }

    fn same_client_as(&self, object_id: &ObjectId) -> bool {
        match self {
            Self::Window(w) => w.same_client_as(object_id),
            Self::LayerSurface(l) => l.wl_surface().id().same_client_as(object_id),
        }
    }
}

impl PointerTarget<State> for PointerFocusTarget {
    fn enter(&self, seat: &Seat<State>, data: &mut State, event: &MotionEvent) {
        self.as_pointer_target().enter(seat, data, event);
    }

    fn motion(&self, seat: &Seat<State>, data: &mut State, event: &MotionEvent) {
        self.as_pointer_target().motion(seat, data, event);
    }

    fn relative_motion(
        &self,
        seat: &Seat<State>,
        data: &mut State,
        event: &RelativeMotionEvent,
    ) {
        self.as_pointer_target().relative_motion(seat, data, event);
    }

    fn button(&self, seat: &Seat<State>, data: &mut State, event: &ButtonEvent) {
        self.as_pointer_target().button(seat, data, event);
    }

    fn axis(&self, seat: &Seat<State>, data: &mut State, frame: AxisFrame) {
        self.as_pointer_target().axis(seat, data, frame);
    }

    fn frame(&self, seat: &Seat<State>, data: &mut State) {
        self.as_pointer_target().frame(seat, data);
    }

    fn leave(
        &self,
        seat: &Seat<State>,
        data: &mut State,
        serial: smithay::utils::Serial,
        time: u32,
    ) {
        self.as_pointer_target().leave(seat, data, serial, time);
    }

    fn gesture_swipe_begin(
        &self,
        seat: &Seat<State>,
        data: &mut State,
        event: &GestureSwipeBeginEvent,
    ) {
        self.as_pointer_target().gesture_swipe_begin(seat, data, event);
    }

    fn gesture_swipe_update(
        &self,
        seat: &Seat<State>,
        data: &mut State,
        event: &GestureSwipeUpdateEvent,
    ) {
        self.as_pointer_target().gesture_swipe_update(seat, data, event);
    }

    fn gesture_swipe_end(
        &self,
        seat: &Seat<State>,
        data: &mut State,
        event: &GestureSwipeEndEvent,
    ) {
        self.as_pointer_target().gesture_swipe_end(seat, data, event);
    }

    fn gesture_pinch_begin(
        &self,
        seat: &Seat<State>,
        data: &mut State,
        event: &GesturePinchBeginEvent,
    ) {
        self.as_pointer_target().gesture_pinch_begin(seat, data, event);
    }

    fn gesture_pinch_update(
        &self,
        seat: &Seat<State>,
        data: &mut State,
        event: &GesturePinchUpdateEvent,
    ) {
        self.as_pointer_target().gesture_pinch_update(seat, data, event);
    }

    fn gesture_pinch_end(
        &self,
        seat: &Seat<State>,
        data: &mut State,
        event: &GesturePinchEndEvent,
    ) {
        self.as_pointer_target().gesture_pinch_end(seat, data, event);
    }

    fn gesture_hold_begin(
        &self,
        seat: &Seat<State>,
        data: &mut State,
        event: &GestureHoldBeginEvent,
    ) {
        self.as_pointer_target().gesture_hold_begin(seat, data, event);
    }

    fn gesture_hold_end(
        &self,
        seat: &Seat<State>,
        data: &mut State,
        event: &GestureHoldEndEvent,
    ) {
        self.as_pointer_target().gesture_hold_end(seat, data, event);
    }
}

impl TouchTarget<State> for PointerFocusTarget {
    fn down(
        &self,
        seat: &Seat<State>,
        data: &mut State,
        event: &smithay::input::touch::DownEvent,
        seq: smithay::utils::Serial,
    ) {
        self.as_touch_target().down(seat, data, event, seq);
    }

    fn up(
        &self,
        seat: &Seat<State>,
        data: &mut State,
        event: &smithay::input::touch::UpEvent,
        seq: smithay::utils::Serial,
    ) {
        self.as_touch_target().up(seat, data, event, seq);
    }

    fn motion(
        &self,
        seat: &Seat<State>,
        data: &mut State,
        event: &smithay::input::touch::MotionEvent,
        seq: smithay::utils::Serial,
    ) {
        self.as_touch_target().motion(seat, data, event, seq);
    }

    fn frame(&self, seat: &Seat<State>, data: &mut State, seq: smithay::utils::Serial) {
        self.as_touch_target().frame(seat, data, seq);
    }

    fn cancel(&self, seat: &Seat<State>, data: &mut State, seq: smithay::utils::Serial) {
        self.as_touch_target().cancel(seat, data, seq);
    }

    fn shape(
        &self,
        seat: &Seat<State>,
        data: &mut State,
        event: &smithay::input::touch::ShapeEvent,
        seq: smithay::utils::Serial,
    ) {
        self.as_touch_target().shape(seat, data, event, seq);
    }

    fn orientation(
        &self,
        seat: &Seat<State>,
        data: &mut State,
        event: &smithay::input::touch::OrientationEvent,
        seq: smithay::utils::Serial,
    ) {
        self.as_touch_target().orientation(seat, data, event, seq);
    }
}

// DndFocus — delegate to the underlying WlSurface for drag-and-drop hit testing.
impl DndFocus<State> for PointerFocusTarget {
    type OfferData<S: Source> = WlOfferData<S>;

    fn enter<S: Source>(
        &self,
        data: &mut State,
        dh: &DisplayHandle,
        source: Arc<S>,
        seat: &Seat<State>,
        location: Point<f64, Logical>,
        serial: &Serial,
    ) -> Option<WlOfferData<S>> {
        let surface = self.wl_surface()?;
        DndFocus::enter(surface.as_ref(), data, dh, source, seat, location, serial)
    }

    fn motion<S: Source>(
        &self,
        data: &mut State,
        offer: Option<&mut WlOfferData<S>>,
        seat: &Seat<State>,
        location: Point<f64, Logical>,
        time: u32,
    ) {
        if let Some(surface) = self.wl_surface() {
            DndFocus::motion(surface.as_ref(), data, offer, seat, location, time);
        }
    }

    fn leave<S: Source>(
        &self,
        data: &mut State,
        offer: Option<&mut WlOfferData<S>>,
        seat: &Seat<State>,
    ) {
        if let Some(surface) = self.wl_surface() {
            DndFocus::leave(surface.as_ref(), data, offer, seat);
        }
    }

    fn drop<S: Source>(
        &self,
        data: &mut State,
        offer: Option<&mut WlOfferData<S>>,
        seat: &Seat<State>,
    ) {
        if let Some(surface) = self.wl_surface() {
            DndFocus::drop(surface.as_ref(), data, offer, seat);
        }
    }
}

impl From<Window> for KeyboardFocusTarget {
    fn from(w: Window) -> Self {
        Self::Window(Box::new(w))
    }
}

impl From<LayerSurface> for KeyboardFocusTarget {
    fn from(l: LayerSurface) -> Self {
        Self::LayerSurface(l)
    }
}

impl From<PopupKind> for KeyboardFocusTarget {
    fn from(p: PopupKind) -> Self {
        Self::Popup(Box::new(p))
    }
}

impl From<Window> for PointerFocusTarget {
    fn from(w: Window) -> Self {
        Self::Window(Box::new(w))
    }
}

impl From<LayerSurface> for PointerFocusTarget {
    fn from(l: LayerSurface) -> Self {
        Self::LayerSurface(l)
    }
}
