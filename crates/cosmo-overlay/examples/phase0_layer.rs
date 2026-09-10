//! Phase 0 probe: one wlr-layer-shell surface anchored bottom-center on the
//! running compositor (cosmic-comp). Renders a solid bar for 6 seconds, then
//! exits 0.
//!
//! Layer::Top, `KeyboardInteractivity::None` — no focus steal. This is the
//! raw `smithay-client-toolkit` path the blueprint lists as overlay option 2
//! (proven by cosmic-voice's candidate window); the libcosmic path decision
//! comes in phase 6.

use std::time::{Duration, Instant};

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use wayland_client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
};

const WIDTH: u32 = 640;
const HEIGHT: u32 = 120;
const BOTTOM_MARGIN: i32 = 48;
const SHOW_FOR: Duration = Duration::from_secs(6);

/// Warm orange, opaque. ARGB8888.
const COLOR: u32 = 0xFF_E8_83_3A;

fn main() {
    let conn = Connection::connect_to_env().expect("connect to Wayland display");
    let (globals, mut event_queue) = registry_queue_init(&conn).expect("registry init");
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor unavailable");
    let layer_shell =
        LayerShell::bind(&globals, &qh).expect("layer shell unavailable on this compositor");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm unavailable");

    let surface = compositor.create_surface(&qh);
    let layer =
        layer_shell.create_layer_surface(&qh, surface, Layer::Top, Some("cosmo-phase0"), None);
    layer.set_anchor(Anchor::BOTTOM);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.set_margin(0, 0, BOTTOM_MARGIN, 0);
    layer.set_size(WIDTH, HEIGHT);
    layer.commit();

    let pool = SlotPool::new((WIDTH * HEIGHT * 4) as usize, &shm).expect("create pool");

    let mut app = Phase0Layer {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        shm,
        pool,
        layer,
        exit: false,
        first_configure: true,
        width: WIDTH,
        height: HEIGHT,
        start: Instant::now(),
    };

    loop {
        event_queue.blocking_dispatch(&mut app).expect("dispatch");
        if app.exit {
            println!(
                "PASS: layer surface rendered bottom-center ({}x{}, {}px margin) for {}s",
                WIDTH,
                HEIGHT,
                BOTTOM_MARGIN,
                SHOW_FOR.as_secs()
            );
            break;
        }
    }
}

struct Phase0Layer {
    registry_state: RegistryState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    exit: bool,
    first_configure: bool,
    width: u32,
    height: u32,
    start: Instant,
}

impl Phase0Layer {
    fn draw(&mut self, qh: &QueueHandle<Self>) {
        let width = self.width as i32;
        let height = self.height as i32;
        let stride = width * 4;

        let (buffer, canvas) = self
            .pool
            .create_buffer(width, height, stride, wl_shm::Format::Argb8888)
            .expect("create buffer");

        for px in canvas.chunks_exact_mut(4) {
            px.copy_from_slice(&COLOR.to_le_bytes());
        }

        self.layer.wl_surface().damage_buffer(0, 0, width, height);
        self.layer
            .wl_surface()
            .frame(qh, FrameCallbackData(self.layer.wl_surface().clone()));
        buffer
            .attach_to(self.layer.wl_surface())
            .expect("attach buffer");
        self.layer.commit();
    }
}

impl CompositorHandler for Phase0Layer {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        if self.start.elapsed() >= SHOW_FOR {
            self.exit = true;
            return;
        }
        self.draw(qh);
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for Phase0Layer {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for Phase0Layer {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        // Anchored to an edge only: our requested size is respected.
        self.width = std::num::NonZeroU32::new(configure.new_size.0)
            .map_or(WIDTH, std::num::NonZeroU32::get);
        self.height = std::num::NonZeroU32::new(configure.new_size.1)
            .map_or(HEIGHT, std::num::NonZeroU32::get);
        if self.first_configure {
            self.first_configure = false;
            println!("configured; rendering...");
            self.draw(qh);
        }
    }
}

impl ShmHandler for Phase0Layer {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_registry!(Phase0Layer);

impl ProvidesRegistryState for Phase0Layer {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

smithay_client_toolkit::delegate_dispatch2!(Phase0Layer);
