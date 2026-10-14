//! Backend DRM/KMS : le compositeur pilote directement l'écran depuis un tty.
//! Session logind via libseat, entrées via libinput, un seul GPU.
//! Variables : ODYSSEUS_OUTPUT (connecteur à utiliser, ex. HDMI-A-1),
//! ODYSSEUS_SCALE (échelle, 1.5 par défaut).

use std::{collections::HashMap, time::Duration};

use smithay::{
    backend::{
        allocator::{
            Fourcc,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode,
            compositor::FrameFlags,
            exporter::gbm::GbmFramebufferExporter,
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
        },
        egl::{EGLContext, EGLDisplay},
        input::InputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{ImportDma, gles::GlesRenderer},
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev::primary_gpu,
    },
    desktop::layer_map_for_output,
    output::{Mode as WlMode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        calloop::{
            EventLoop, LoopHandle,
            timer::{TimeoutAction, Timer},
        },
        drm::control::{Device as ControlDevice, ModeTypeFlags, connector, crtc},
        input::Libinput,
        rustix::fs::OFlags,
    },
    utils::{DeviceFd, Transform},
};

use crate::{
    CalloopData,
    cursor::{Cursor, CursorElement},
    render::{OdysseusElement, Shaders},
    state::ModKey,
    wallpaper::Wallpaper,
};
use common::theme;

type OutputManager = DrmOutputManager<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    (),
    DrmDeviceFd,
>;
type Surface =
    DrmOutput<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

const COLOR_FORMATS: &[Fourcc] = &[Fourcc::Abgr8888, Fourcc::Argb8888];

struct OutputSurface {
    output: Output,
    drm_output: Surface,
    /// Une image est en attente d'affichage : ne pas en rendre une autre avant le vblank
    pending: bool,
}

pub struct UdevData {
    handle: LoopHandle<'static, CalloopData>,
    manager: OutputManager,
    renderer: GlesRenderer,
    shaders: Shaders,
    wallpaper: Option<Wallpaper>,
    cursor: Option<Cursor>,
    surfaces: HashMap<crtc::Handle, OutputSurface>,
    /// Session inactive (autre tty au premier plan) : pas de rendu
    paused: bool,
}

pub fn init_udev(
    event_loop: &mut EventLoop<'static, CalloopData>,
    data: &mut CalloopData,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut session, notifier) = LibSeatSession::new()?;
    let seat = session.seat();

    // GPU principal et ouverture du périphérique via logind
    let gpu_path = primary_gpu(&seat)?.ok_or("aucun GPU trouvé")?;
    let fd = session.open(
        &gpu_path,
        OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
    )?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let node = DrmNode::from_file(&fd)?;
    tracing::info!("GPU : {}", gpu_path.display());

    let (drm, drm_notifier) = DrmDevice::new(fd.clone(), true)?;
    let gbm = GbmDevice::new(fd)?;
    let display = unsafe { EGLDisplay::new(gbm.clone())? };
    let context = EGLContext::new(&display)?;
    let mut renderer = unsafe { GlesRenderer::new(context)? };
    let shaders = Shaders::compile(&mut renderer)?;
    {
        use smithay::backend::renderer::Renderer;
        data.state.renderer_context = Some(renderer.context_id());
    }
    let wallpaper =
        crate::wallpaper::find().and_then(|path| Wallpaper::load(&mut renderer, &path).ok());

    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let exporter = GbmFramebufferExporter::new(gbm.clone(), Some(node));
    let render_formats = renderer.dmabuf_formats();
    let mut manager = OutputManager::new(
        drm,
        allocator,
        exporter,
        Some(gbm),
        COLOR_FORMATS.iter().copied(),
        render_formats,
    );

    data.state.mod_key = ModKey::Super;
    let scale: f64 = std::env::var("ODYSSEUS_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1.5);
    let cursor_size = std::env::var("XCURSOR_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(24u32);
    let cursor = Cursor::load((cursor_size as f64 * scale).round() as u32);

    // Écrans : ODYSSEUS_OUTPUT s'il est défini, sinon le premier connecté
    let wanted = std::env::var("ODYSSEUS_OUTPUT").ok();
    let mut surfaces = HashMap::new();
    let res = manager.device().resource_handles()?;
    let mut used_crtcs = Vec::new();
    for handle in res.connectors() {
        let info = manager.device().get_connector(*handle, true)?;
        if info.state() != connector::State::Connected || info.modes().is_empty() {
            continue;
        }
        let name = format!("{}-{}", info.interface().as_str(), info.interface_id());
        if wanted.as_deref().is_some_and(|w| w != name)
            || (wanted.is_none() && !surfaces.is_empty())
        {
            tracing::info!("écran {name} ignoré");
            continue;
        }
        // Premier CRTC compatible avec un encodeur du connecteur
        let crtc = info
            .encoders()
            .iter()
            .filter_map(|e| manager.device().get_encoder(*e).ok())
            .flat_map(|enc| res.filter_crtcs(enc.possible_crtcs()))
            .find(|c| !used_crtcs.contains(c));
        let Some(crtc) = crtc else {
            tracing::warn!("pas de CRTC libre pour {name}");
            continue;
        };
        used_crtcs.push(crtc);

        let drm_mode = info
            .modes()
            .iter()
            .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
            .copied()
            .unwrap_or(info.modes()[0]);
        let wl_mode = WlMode::from(drm_mode);
        let (w_mm, h_mm) = info.size().unwrap_or((0, 0));
        let output = Output::new(
            name.clone(),
            PhysicalProperties {
                size: (w_mm as i32, h_mm as i32).into(),
                subpixel: Subpixel::Unknown,
                make: "odysseus".into(),
                model: name.clone(),
            },
        );
        let _global = output.create_global::<crate::Odysseus>(&data.display_handle);
        output.change_current_state(
            Some(wl_mode),
            Some(Transform::Normal),
            Some(Scale::Fractional(scale)),
            Some((0, 0).into()),
        );
        output.set_preferred(wl_mode);
        data.state.space.map_output(&output, (0, 0));

        let planes = manager.device().planes(&crtc).ok();
        let drm_output = manager
            .initialize_output::<_, OdysseusElement>(
                crtc,
                drm_mode,
                &[*handle],
                &output,
                planes,
                &mut renderer,
                &DrmOutputRenderElements::default(),
            )
            .map_err(|e| format!("initialisation de {name} : {e:?}"))?;
        tracing::info!(
            "écran {name} : {}x{} @ {} mHz, échelle {scale}",
            wl_mode.size.w,
            wl_mode.size.h,
            wl_mode.refresh
        );
        surfaces.insert(
            crtc,
            OutputSurface {
                output,
                drm_output,
                pending: false,
            },
        );
    }
    if surfaces.is_empty() {
        return Err("aucun écran utilisable".into());
    }

    // Entrées
    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&seat)
        .map_err(|_| "libinput : siège introuvable")?;
    event_loop.handle().insert_source(
        LibinputInputBackend::new(libinput.clone()),
        |event, _, data| {
            if let InputEvent::DeviceAdded { .. } | InputEvent::DeviceRemoved { .. } = event {
                return;
            }
            data.state.process_input_event(event);
        },
    )?;

    // Synchronisation verticale : l'image précédente est affichée, on peut rendre la suivante
    event_loop
        .handle()
        .insert_source(drm_notifier, |event, _, data| match event {
            DrmEvent::VBlank(crtc) => {
                if let Some(udev) = data.udev.as_mut()
                    && let Some(surface) = udev.surfaces.get_mut(&crtc)
                {
                    surface.pending = false;
                    if let Err(e) = surface.drm_output.frame_submitted() {
                        tracing::warn!("frame_submitted : {e:?}");
                    }
                }
                render(data, crtc);
            }
            DrmEvent::Error(e) => tracing::error!("DRM : {e}"),
        })?;

    // Changement de tty : suspendre puis reprendre l'affichage et les entrées
    event_loop
        .handle()
        .insert_source(notifier, move |event, _, data| {
            let Some(udev) = data.udev.as_mut() else {
                return;
            };
            match event {
                SessionEvent::PauseSession => {
                    libinput.suspend();
                    udev.manager.pause();
                    udev.paused = true;
                }
                SessionEvent::ActivateSession => {
                    if libinput.resume().is_err() {
                        tracing::warn!("libinput : reprise impossible");
                    }
                    if let Err(e) = udev.manager.activate(false) {
                        tracing::error!("réactivation DRM : {e}");
                    }
                    udev.paused = false;
                    for surface in udev.surfaces.values_mut() {
                        surface.pending = false;
                    }
                    let crtcs: Vec<_> = udev.surfaces.keys().copied().collect();
                    for crtc in crtcs {
                        render(data, crtc);
                    }
                }
            }
        })?;

    let crtcs: Vec<_> = surfaces.keys().copied().collect();
    data.state.session = Some(session.clone());
    data.udev = Some(UdevData {
        handle: event_loop.handle(),
        manager,
        renderer,
        shaders,
        wallpaper,
        cursor,
        surfaces,
        paused: false,
    });
    for crtc in crtcs {
        render(data, crtc);
    }
    Ok(())
}

/// Rend une image pour un écran ; sans changement à l'écran, réessaie après ~1 image.
fn render(data: &mut CalloopData, crtc: crtc::Handle) {
    let Some(udev) = data.udev.as_mut() else {
        return;
    };
    let state = &mut data.state;
    let Some(surface) = udev.surfaces.get_mut(&crtc) else {
        return;
    };
    if surface.pending || udev.paused {
        return;
    }
    let output = surface.output.clone();
    let Some(mode) = output.current_mode() else {
        return;
    };
    let scale = output.current_scale().fractional_scale();
    let output_loc = state
        .space
        .output_geometry(&output)
        .map(|g| g.loc)
        .unwrap_or_default();

    // Curseur au-dessus de tout
    let mut elements: Vec<OdysseusElement> = Vec::new();
    if let Some(cursor) = udev.cursor.as_ref() {
        let pos = state.seat.get_pointer().unwrap().current_location() - output_loc.to_f64();
        match cursor.element(&mut udev.renderer, &mut state.cursor_status, pos, scale) {
            Some(CursorElement::Theme(e)) => elements.push(OdysseusElement::CursorTheme(e)),
            Some(CursorElement::Surface(list)) => {
                elements.extend(list.into_iter().map(OdysseusElement::Layer))
            }
            None => {}
        }
    }
    let focused = state.seat.get_keyboard().unwrap().current_focus();
    elements.extend(crate::render::output_elements(
        &state.space,
        &output,
        &mut udev.renderer,
        &udev.shaders,
        focused.as_ref(),
        mode.size,
        udev.wallpaper.as_ref(),
        &state.opening,
        &state.closing,
        state.top_is_fullscreen(),
    ));

    if state.capture_pending || crate::capture::requested_by_file() {
        state.capture_pending = false;
        match crate::capture::save(&mut udev.renderer, &elements, mode.size, scale) {
            Ok(path) => tracing::info!("capture : {}", path.display()),
            Err(e) => tracing::warn!("capture impossible : {e}"),
        }
    }

    let bg = theme::BACKGROUND;
    let rendered = match surface.drm_output.render_frame(
        &mut udev.renderer,
        &elements,
        [bg.r, bg.g, bg.b, 1.0],
        FrameFlags::DEFAULT,
    ) {
        Ok(result) if !result.is_empty => match surface.drm_output.queue_frame(()) {
            Ok(()) => {
                surface.pending = true;
                true
            }
            Err(e) => {
                tracing::warn!("queue_frame : {e:?}");
                false
            }
        },
        Ok(_) => false,
        Err(e) => {
            tracing::warn!("render_frame : {e:?}");
            false
        }
    };
    if !rendered {
        let delay = Duration::from_micros(1_000_000_000 / mode.refresh.max(1) as u64);
        let _ = udev
            .handle
            .insert_source(Timer::from_duration(delay), move |_, _, data| {
                render(data, crtc);
                TimeoutAction::Drop
            });
    }

    state.after_frame(&output);
    layer_map_for_output(&output).cleanup();
}
