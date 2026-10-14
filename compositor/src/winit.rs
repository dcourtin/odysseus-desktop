use smithay::{
    backend::{
        renderer::damage::OutputDamageTracker,
        winit::{self, WinitEvent},
    },
    desktop::layer_map_for_output,
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::calloop::EventLoop,
    utils::{Rectangle, Transform},
};

use crate::{CalloopData, Odysseus};
use common::theme;

pub fn init_winit(
    event_loop: &mut EventLoop<CalloopData>,
    data: &mut CalloopData,
) -> Result<(), Box<dyn std::error::Error>> {
    let display_handle = &mut data.display_handle;
    let state = &mut data.state;

    let (mut backend, winit) = winit::init()?;
    let shaders = crate::render::Shaders::compile(backend.renderer())?;
    {
        use smithay::backend::renderer::Renderer;
        state.renderer_context = Some(backend.renderer().context_id());
    }
    let wallpaper =
        crate::wallpaper::find().and_then(|path| {
            match crate::wallpaper::Wallpaper::load(backend.renderer(), &path) {
                Ok(w) => Some(w),
                Err(e) => {
                    tracing::warn!("fond d'écran {} : {e}", path.display());
                    None
                }
            }
        });

    // linux-dmabuf pour les clients GL
    {
        use smithay::backend::{egl::EGLDevice, renderer::ImportDma};
        let formats = backend.renderer().dmabuf_formats();
        let node = EGLDevice::device_for_display(backend.renderer().egl_context().display())
            .ok()
            .and_then(|device| device.try_get_render_node().ok().flatten());
        let feedback = node.and_then(|node| {
            smithay::wayland::dmabuf::DmabufFeedbackBuilder::new(node.dev_id(), formats.clone())
                .build()
                .ok()
        });
        let global = match feedback {
            Some(feedback) => state
                .dmabuf_state
                .create_global_with_default_feedback::<Odysseus>(display_handle, &feedback),
            None => state
                .dmabuf_state
                .create_global::<Odysseus>(display_handle, formats),
        };
        state.dmabuf_global = Some(global);
    }

    let mode = Mode {
        size: backend.window_size(),
        refresh: 60_000,
    };

    let output = Output::new(
        "winit".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Smithay".into(),
            model: "Winit".into(),
        },
    );
    let _global = output.create_global::<Odysseus>(display_handle);
    // Même échelle que l'écran hôte (1.5 sous sway) : les clients reçoivent l'entier supérieur
    let scale = Scale::Fractional(backend.window().scale_factor());
    output.change_current_state(
        Some(mode),
        Some(Transform::Flipped180),
        Some(scale),
        Some((0, 0).into()),
    );
    output.set_preferred(mode);

    state.space.map_output(&output, (0, 0));

    let mut damage_tracker = OutputDamageTracker::from_output(&output);

    // WAYLAND_DISPLAY est transmis aux programmes lancés (voir Odysseus::spawn)

    event_loop
        .handle()
        .insert_source(winit, move |event, _, data| {
            let state = &mut data.state;

            match event {
                WinitEvent::Resized { size, scale_factor } => {
                    output.change_current_state(
                        Some(Mode {
                            size,
                            refresh: 60_000,
                        }),
                        None,
                        Some(Scale::Fractional(scale_factor)),
                        None,
                    );
                    layer_map_for_output(&output).arrange();
                }
                WinitEvent::Input(event) => state.process_input_event(event),
                WinitEvent::Redraw => {
                    let size = backend.window_size();
                    let damage = Rectangle::from_size(size);

                    {
                        let focused = state.seat.get_keyboard().unwrap().current_focus();
                        let (renderer, mut framebuffer) = backend.bind().unwrap();
                        let elements = match crate::lock::lock_elements(state, &output, renderer) {
                            Some(lock) => lock,
                            None => crate::render::output_elements(
                                &state.space,
                                &output,
                                renderer,
                                &shaders,
                                focused.as_ref(),
                                size,
                                wallpaper.as_ref(),
                                &state.opening,
                                &state.closing,
                                state.top_is_fullscreen(),
                            ),
                        };
                        damage_tracker
                            .render_output(
                                renderer,
                                &mut framebuffer,
                                0,
                                &elements,
                                [
                                    theme::BACKGROUND.r,
                                    theme::BACKGROUND.g,
                                    theme::BACKGROUND.b,
                                    1.0,
                                ],
                            )
                            .unwrap();
                    }
                    backend.submit(Some(&[damage])).unwrap();

                    state.after_frame(&output);
                    layer_map_for_output(&output).cleanup();

                    // Ask for redraw to schedule new frame.
                    backend.window().request_redraw();
                }
                WinitEvent::CloseRequested => {
                    state.loop_signal.stop();
                }
                _ => (),
            };
        })?;

    Ok(())
}
