//! Shared Wayland display/socket bootstrap.

use anyhow::{Context, Result};
use smithay::{
    reexports::{
        calloop::{
            generic::Generic, EventLoop, Interest, LoopSignal, Mode as CalloopMode, PostAction,
        },
        wayland_server::{Display, DisplayHandle},
    },
    wayland::socket::ListeningSocketSource,
};
use std::{ffi::OsString, sync::Arc};
use tracing::info;

use crate::wayland_state::{ClientState, SpikeState};

pub struct WaylandRuntime {
    pub event_loop: EventLoop<'static, SpikeState>,
    pub loop_signal: LoopSignal,
    pub display_handle: DisplayHandle,
    pub socket_name: OsString,
    pub state: SpikeState,
}

impl WaylandRuntime {
    pub fn new() -> Result<Self> {
        let event_loop =
            EventLoop::<SpikeState>::try_new().context("failed to create calloop event loop")?;
        let loop_signal = event_loop.get_signal();

        let display = Display::<SpikeState>::new().context("failed to create wayland display")?;
        let display_handle = display.handle();

        let socket_source =
            ListeningSocketSource::new_auto().context("failed to bind wayland socket")?;
        let socket_name = socket_source.socket_name().to_os_string();
        info!("Wayland socket: {:?}", socket_name);
        println!("WAYLAND_DISPLAY={}", socket_name.to_string_lossy());

        event_loop
            .handle()
            .insert_source(
                Generic::new(display, Interest::READ, CalloopMode::Level),
                |_event, display, state| {
                    unsafe { display.get_mut().dispatch_clients(state)? };
                    Ok(PostAction::Continue)
                },
            )
            .context("failed to insert wayland source")?;

        event_loop
            .handle()
            .insert_source(socket_source, |stream, _, state| {
                state
                    .display_handle
                    .insert_client(stream, Arc::new(ClientState::default()))
                    .unwrap();
            })
            .context("failed to insert socket source")?;

        info!("Wayland socket ready");

        let state = SpikeState::new(
            display_handle.clone(),
            event_loop.handle(),
            loop_signal.clone(),
        );

        Ok(Self {
            event_loop,
            loop_signal,
            display_handle,
            socket_name,
            state,
        })
    }
}
