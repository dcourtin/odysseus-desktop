//! Backend DRM/KMS : le compositeur pilote directement l'écran depuis un tty.
//! Session logind via libseat, entrées via libinput, un seul GPU, un écran.
//! Variables : ODYSSEUS_OUTPUT (connecteur préféré, ex. HDMI-A-1 ; sinon le
//! premier connecté), ODYSSEUS_SCALE (échelle, 1.5 par défaut).
//! Écrans branchés à chaud : udev signale le changement, on réexamine les
//! connecteurs (l'écran préféré revient dès qu'il est rebranché).
//! Par IPC : `output scale 1.25`, `output mode 2560x1440@60`.

use std::os::fd::AsFd;
use std::{collections::HashMap, time::Duration};

use smithay::{
    backend::{
        allocator::{
            Fourcc,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, NodeType,
            compositor::FrameFlags,
            exporter::gbm::GbmFramebufferExporter,
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
        },
        egl::{EGLContext, EGLDisplay},
        input::InputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{ImportDma, gles::GlesRenderer},
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev::{UdevBackend, UdevEvent, primary_gpu},
    },
    desktop::layer_map_for_output,
    output::{Mode as WlMode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        calloop::{
            EventLoop, Interest, LoopHandle, Mode, PostAction,
            generic::Generic,
            timer::{TimeoutAction, Timer},
        },
        drm::control::{Device as ControlDevice, ModeTypeFlags, connector, crtc},
        input::Libinput,
        rustix::fs::OFlags,
        wayland_server::backend::GlobalId,
    },
    utils::{DeviceFd, Transform},
    wayland::dmabuf::DmabufFeedbackBuilder,
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
    connector: connector::Handle,
    global: GlobalId,
    /// Taille de la table gamma du CRTC (lumière de nuit)
    gamma_size: usize,
    /// Une image est en attente d'affichage : ne pas en rendre une autre avant le vblank
    pending: bool,
    /// Écran éteint (veille)
    blanked: bool,
}

pub struct UdevData {
    handle: LoopHandle<'static, CalloopData>,
    manager: OutputManager,
    renderer: GlesRenderer,
    shaders: Shaders,
    wallpaper: Option<Wallpaper>,
    cursor: Option<Cursor>,
    cursor_size: u32,
    surfaces: HashMap<crtc::Handle, OutputSurface>,
    /// Session inactive (autre tty au premier plan) : pas de rendu
    paused: bool,
    /// Écran préféré (ODYSSEUS_OUTPUT) et échelle courante
    wanted: Option<String>,
    scale: f64,
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
    let wallpaper = crate::wallpaper::find(&data.state.settings)
        .and_then(|path| Wallpaper::load(&mut renderer, &path).ok());

    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let exporter = GbmFramebufferExporter::new(gbm.clone(), Some(node));
    let render_formats = renderer.dmabuf_formats();

    // linux-dmabuf : les clients GL (Firefox, mpv…) partagent leurs tampons GPU
    let render_node = node
        .node_with_type(NodeType::Render)
        .and_then(Result::ok)
        .unwrap_or(node);
    match DmabufFeedbackBuilder::new(render_node.dev_id(), render_formats.clone()).build() {
        Ok(feedback) => {
            let global = data
                .state
                .dmabuf_state
                .create_global_with_default_feedback::<crate::Odysseus>(
                    &data.display_handle,
                    &feedback,
                );
            data.state.dmabuf_global = Some(global);
        }
        Err(e) => tracing::warn!("linux-dmabuf indisponible : {e}"),
    }
    let manager = OutputManager::new(
        drm,
        allocator,
        exporter,
        Some(gbm),
        COLOR_FORMATS.iter().copied(),
        render_formats,
    );

    data.state.mod_key = ModKey::Super;
    // Échelle et mode : Réglages > Écran, sinon ODYSSEUS_SCALE et le mode préféré
    let scale: f64 = data.state.settings.scale.unwrap_or_else(|| {
        std::env::var("ODYSSEUS_SCALE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.5)
    });
    data.state.pending_mode = data
        .state
        .settings
        .mode
        .as_deref()
        .and_then(crate::ipc::parse_mode);
    let cursor_size = std::env::var("XCURSOR_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(24u32);
    let cursor = Cursor::load((cursor_size as f64 * scale).round() as u32);

    data.udev = Some(UdevData {
        handle: event_loop.handle(),
        manager,
        renderer,
        shaders,
        wallpaper,
        cursor,
        cursor_size,
        surfaces: HashMap::new(),
        paused: false,
        wanted: std::env::var("ODYSSEUS_OUTPUT").ok(),
        scale,
    });
    scan_outputs(data);
    if data.udev.as_ref().is_some_and(|u| u.surfaces.is_empty()) {
        return Err("aucun écran utilisable".into());
    }

    // Écran branché ou débranché
    let udev_backend = UdevBackend::new(&seat)?;
    event_loop
        .handle()
        .insert_source(udev_backend, |event, _, data| {
            if let UdevEvent::Changed { .. } | UdevEvent::Added { .. } = event {
                scan_outputs(data);
            }
        })?;

    // Entrées
    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&seat)
        .map_err(|_| "libinput : siège introuvable")?;
    event_loop.handle().insert_source(
        LibinputInputBackend::new(libinput.clone()),
        |event, _, data| {
            match &event {
                InputEvent::DeviceAdded { device } | InputEvent::DeviceRemoved { device } => {
                    use smithay::reexports::input::DeviceCapability;
                    let apple = device.id_vendor() == 0x05ac
                        && device.has_capability(DeviceCapability::Keyboard);
                    let added = matches!(event, InputEvent::DeviceAdded { .. });
                    if apple {
                        tracing::info!(
                            "clavier Apple {} : {}",
                            if added { "branché" } else { "retiré" },
                            device.name()
                        );
                    }
                    data.state.keyboard_device_changed(apple, added);
                    return;
                }
                _ => {}
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

    // Thème changé (centre de contrôle) : bordures et accents suivent
    if let Some(fd) = theme::watch() {
        event_loop.handle().insert_source(
            Generic::new(fd, Interest::READ, Mode::Level),
            |_, fd, data| {
                theme::drain(fd.as_fd());
                let theme_changed = theme::reload();
                // Réglages modifiés : fond d'écran rechargé s'il a changé
                let wallpaper_changed = data.state.reload_settings();
                if let Some(udev) = data.udev.as_mut() {
                    if wallpaper_changed {
                        udev.wallpaper = crate::wallpaper::find(&data.state.settings)
                            .and_then(|path| Wallpaper::load(&mut udev.renderer, &path).ok());
                    }
                    if theme_changed || wallpaper_changed {
                        let crtcs: Vec<_> = udev.surfaces.keys().copied().collect();
                        for crtc in crtcs {
                            render(data, crtc);
                        }
                    }
                }
                Ok(PostAction::Continue)
            },
        )?;
    }

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
                    // La table gamma a pu être réinitialisée par un autre programme
                    data.state.night.applied = None;
                    // Un écran a pu être branché ou débranché pendant ce temps
                    scan_outputs(data);
                    let crtcs: Vec<_> = data
                        .udev
                        .as_ref()
                        .map(|u| u.surfaces.keys().copied().collect())
                        .unwrap_or_default();
                    for crtc in crtcs {
                        render(data, crtc);
                    }
                }
            }
        })?;

    data.state.session = Some(session.clone());
    let crtcs: Vec<_> = data
        .udev
        .as_ref()
        .map(|u| u.surfaces.keys().copied().collect())
        .unwrap_or_default();
    for crtc in crtcs {
        render(data, crtc);
    }
    Ok(())
}

fn connector_name(info: &connector::Info) -> String {
    format!("{}-{}", info.interface().as_str(), info.interface_id())
}

/// Réexamine les connecteurs : garde l'écran actuel s'il est toujours là,
/// sinon passe à l'écran préféré (ou au premier connecté).
fn scan_outputs(data: &mut CalloopData) {
    let CalloopData {
        state,
        display_handle,
        udev,
    } = data;
    let Some(udev) = udev.as_mut() else {
        return;
    };
    if udev.paused {
        return;
    }
    let Ok(res) = udev.manager.device().resource_handles() else {
        return;
    };
    let connected: Vec<connector::Info> = res
        .connectors()
        .iter()
        .filter_map(|h| udev.manager.device().get_connector(*h, true).ok())
        .filter(|i| i.state() == connector::State::Connected && !i.modes().is_empty())
        .collect();
    let chosen = connected
        .iter()
        .find(|i| udev.wanted.as_deref() == Some(connector_name(i).as_str()))
        .or_else(|| connected.first());
    let current = udev.surfaces.values().map(|s| s.connector).next();
    if chosen.map(|i| i.handle()) == current && current.is_some() {
        return;
    }

    // Écran précédent débranché (ou remplacé par l'écran préféré)
    let old: Vec<crtc::Handle> = udev.surfaces.keys().copied().collect();
    for crtc in old {
        if let Some(surface) = udev.surfaces.remove(&crtc) {
            tracing::info!("écran {} retiré", surface.output.name());
            state.space.unmap_output(&surface.output);
            display_handle.remove_global::<crate::Odysseus>(surface.global);
        }
    }
    let Some(info) = chosen else {
        tracing::warn!("aucun écran connecté");
        return;
    };
    let name = connector_name(info);

    // Premier CRTC compatible avec un encodeur du connecteur
    let crtc = info
        .encoders()
        .iter()
        .filter_map(|e| udev.manager.device().get_encoder(*e).ok())
        .flat_map(|enc| res.filter_crtcs(enc.possible_crtcs()))
        .next();
    let Some(crtc) = crtc else {
        tracing::warn!("pas de CRTC libre pour {name}");
        return;
    };
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
    let global = output.create_global::<crate::Odysseus>(display_handle);
    output.change_current_state(
        Some(wl_mode),
        Some(Transform::Normal),
        Some(Scale::Fractional(udev.scale)),
        Some((0, 0).into()),
    );
    output.set_preferred(wl_mode);
    state.space.map_output(&output, (0, 0));

    let planes = udev.manager.device().planes(&crtc).ok();
    let drm_output = match udev.manager.initialize_output::<_, OdysseusElement>(
        crtc,
        drm_mode,
        &[info.handle()],
        &output,
        planes,
        &mut udev.renderer,
        &DrmOutputRenderElements::default(),
    ) {
        Ok(o) => o,
        Err(e) => {
            tracing::error!("initialisation de {name} : {e:?}");
            state.space.unmap_output(&output);
            display_handle.remove_global::<crate::Odysseus>(global);
            return;
        }
    };
    let gamma_size = udev
        .manager
        .device()
        .get_crtc(crtc)
        .map(|c| c.gamma_length() as usize)
        .unwrap_or(0);
    tracing::info!(
        "écran {name} : {}x{} @ {} mHz, échelle {}",
        wl_mode.size.w,
        wl_mode.size.h,
        wl_mode.refresh,
        udev.scale
    );
    let mut modes: Vec<(i32, i32, i32)> = info
        .modes()
        .iter()
        .map(|m| (m.size().0 as i32, m.size().1 as i32, m.vrefresh() as i32))
        .collect();
    modes.sort_by(|a, b| b.cmp(a));
    modes.dedup();
    let preferred = (
        drm_mode.size().0 as i32,
        drm_mode.size().1 as i32,
        drm_mode.vrefresh() as i32,
    );
    state.screen = Some(crate::state::ScreenInfo {
        name: name.clone(),
        modes,
        preferred,
        current: preferred,
        scale: udev.scale,
        size_mm: (w_mm, h_mm),
    });
    udev.surfaces.insert(
        crtc,
        OutputSurface {
            output: output.clone(),
            drm_output,
            connector: info.handle(),
            global,
            gamma_size,
            pending: false,
            blanked: false,
        },
    );
    layer_map_for_output(&output).arrange();
    state.night.applied = None;
    // Fenêtres ramenées dans le nouvel écran si elles en dépassent
    state.fit_windows_to_output();
    let handle = udev.handle.clone();
    let _ = handle.insert_source(Timer::immediate(), move |_, _, data| {
        render(data, crtc);
        TimeoutAction::Drop
    });
}

/// Réglages demandés par IPC (échelle, mode) et lumière de nuit, appliqués
/// avant le rendu d'une image.
fn apply_output_settings(udev: &mut UdevData, state: &mut crate::Odysseus, crtc: crtc::Handle) {
    let Some(surface) = udev.surfaces.get_mut(&crtc) else {
        return;
    };
    if let Some(scale) = state.pending_scale.take() {
        udev.scale = scale;
        surface
            .output
            .change_current_state(None, None, Some(Scale::Fractional(scale)), None);
        layer_map_for_output(&surface.output).arrange();
        udev.cursor = Cursor::load((udev.cursor_size as f64 * scale).round() as u32);
        state.fit_windows_to_output();
        if let Some(screen) = state.screen.as_mut() {
            screen.scale = scale;
        }
        tracing::info!("échelle {scale}");
    }
    if let Some((w, h, refresh)) = state.pending_mode.take() {
        let mode = udev
            .manager
            .device()
            .get_connector(surface.connector, false)
            .ok()
            .and_then(|info| {
                info.modes()
                    .iter()
                    .filter(|m| {
                        let (mw, mh) = m.size();
                        mw as i32 == w
                            && mh as i32 == h
                            && refresh.is_none_or(|r| (m.vrefresh() as i32 - r).abs() <= 1)
                    })
                    .max_by_key(|m| m.vrefresh())
                    .copied()
            });
        match mode {
            Some(mode) => match surface.drm_output.use_mode::<_, OdysseusElement>(
                mode,
                &mut udev.renderer,
                &DrmOutputRenderElements::default(),
            ) {
                Ok(()) => {
                    let wl_mode = WlMode::from(mode);
                    surface
                        .output
                        .change_current_state(Some(wl_mode), None, None, None);
                    surface.output.set_preferred(wl_mode);
                    layer_map_for_output(&surface.output).arrange();
                    state.fit_windows_to_output();
                    surface.pending = false;
                    if let Some(screen) = state.screen.as_mut() {
                        screen.current = (w, h, mode.vrefresh() as i32);
                    }
                    tracing::info!("mode {w}x{h} @ {} Hz", mode.vrefresh());
                }
                Err(e) => tracing::warn!("mode {w}x{h} refusé : {e:?}"),
            },
            None => tracing::warn!("mode {w}x{h} inconnu pour cet écran"),
        }
    }
    if surface.gamma_size > 1
        && let Some(kelvin) = state.night.step()
    {
        let (r, g, b) = crate::night::ramps(kelvin, surface.gamma_size);
        match udev.manager.device().set_gamma(crtc, &r, &g, &b) {
            Ok(()) => state.night.applied = Some(kelvin),
            Err(e) => {
                tracing::warn!("table gamma : {e}");
                // Pas de nouvel essai à chaque image
                state.night.applied = Some(kelvin);
            }
        }
    }
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
    if udev.paused {
        return;
    }
    // Veille : écran éteint (DPMS), on vérifie régulièrement s'il faut le rallumer
    if state.screen_off {
        if !surface.blanked {
            if let Err(e) = surface.drm_output.with_compositor(|c| c.clear()) {
                tracing::warn!("extinction de l'écran : {e}");
            }
            surface.blanked = true;
            surface.pending = false;
        }
        // Rien n'est affiché : le verrouillage peut être confirmé
        state.confirm_lock();
        let _ = udev.handle.insert_source(
            Timer::from_duration(Duration::from_millis(100)),
            move |_, _, data| {
                render(data, crtc);
                TimeoutAction::Drop
            },
        );
        return;
    }
    if surface.blanked {
        state.night.applied = None;
        // Réveil : état DRM remis à zéro, la prochaine image rallume l'écran
        if let Err(e) = surface.drm_output.with_compositor(|c| c.reset_state()) {
            tracing::warn!("rallumage de l'écran : {e}");
        }
        surface.blanked = false;
        surface.pending = false;
    }
    if surface.pending {
        return;
    }
    apply_output_settings(udev, state, crtc);
    let Some(surface) = udev.surfaces.get_mut(&crtc) else {
        return;
    };
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

    // Curseur au-dessus de tout (croix de visée pendant une capture)
    let mut elements: Vec<OdysseusElement> = Vec::new();
    if let Some(cursor) = udev.cursor.as_ref() {
        let pos = state.seat.get_pointer().unwrap().current_location() - output_loc.to_f64();
        let crosshair = state.selection.is_some();
        match cursor.element(
            &mut udev.renderer,
            &mut state.cursor_status,
            pos,
            scale,
            crosshair,
        ) {
            Some(CursorElement::Theme(e)) => elements.push(OdysseusElement::CursorTheme(e)),
            Some(CursorElement::Surface(list)) => {
                elements.extend(list.into_iter().map(OdysseusElement::Layer))
            }
            None => {}
        }
    }
    // Glisser-déposer : l'image du client, sous la pointe du curseur
    if let Some(icon) = state
        .dnd_icon
        .as_ref()
        .filter(|i| smithay::utils::IsAlive::alive(*i))
    {
        let pos = state.seat.get_pointer().unwrap().current_location() - output_loc.to_f64();
        elements.extend(
            smithay::backend::renderer::element::surface::render_elements_from_surface_tree(
                &mut udev.renderer,
                icon,
                pos.to_physical(scale).to_i32_round(),
                smithay::utils::Scale::from(scale),
                1.0,
                smithay::backend::renderer::element::Kind::Unspecified,
            )
            .into_iter()
            .map(OdysseusElement::Layer),
        );
    }
    let focused = state.seat.get_keyboard().unwrap().current_focus();
    // Couche de capture d'écran (voile, sélection, éclair), sous le curseur
    if !state.is_locked() {
        elements.extend(crate::render::overlay_elements(
            state,
            &output,
            &mut udev.renderer,
            &udev.shaders,
            mode.size,
        ));
    }
    // La scène seule (sans curseur ni couche de capture) : c'est ce qu'on capture
    let scene_start = elements.len();
    match crate::lock::lock_elements(state, &output, &mut udev.renderer) {
        Some(lock) => elements.extend(lock),
        None => {
            let scene = state.scene(focused.as_ref());
            elements.extend(crate::render::output_elements(
                &state.space,
                &output,
                &mut udev.renderer,
                &udev.shaders,
                mode.size,
                udev.wallpaper.as_ref(),
                &scene,
            ))
        }
    }

    // Capture par fichier (outils de test) : l'écran tel qu'il est affiché,
    // couche de sélection et curseur compris
    if crate::capture::requested_by_file()
        && let Err(e) =
            crate::capture::save(&mut udev.renderer, &elements, mode.size, scale, None, false)
    {
        tracing::warn!("capture de contrôle impossible : {e}");
    }
    // Capture pour un agent (odysseus-mcp) : écran tel quel, ou une fenêtre
    if let Some(request) = state.agent_capture.take() {
        let window = request.window.and_then(|id| state.window_by_id(id));
        let png = match window {
            Some((window, _)) => {
                let loc = state.space.element_location(&window).unwrap_or_default();
                let only = crate::render::single_window_elements(
                    &window,
                    loc,
                    &output,
                    &mut udev.renderer,
                    &udev.shaders,
                    mode.size,
                );
                let geo = smithay::utils::Rectangle::new(
                    loc.to_f64() - output_loc.to_f64(),
                    window.geometry().size.to_f64(),
                );
                crate::capture::render_png(
                    &mut udev.renderer,
                    &only,
                    mode.size,
                    scale,
                    Some(geo.to_physical(scale).to_i32_round()),
                    true,
                )
            }
            None => crate::capture::render_png(
                &mut udev.renderer,
                &elements[scene_start..],
                mode.size,
                scale,
                None,
                false,
            ),
        };
        // Écriture puis renommage : l'agent ne lit jamais un fichier à moitié écrit
        let tmp = request.path.with_extension("tmp");
        match png.map_err(|e| e.to_string()).and_then(|png| {
            std::fs::write(&tmp, png)
                .and_then(|_| std::fs::rename(&tmp, &request.path))
                .map_err(|e| e.to_string())
        }) {
            Ok(()) => tracing::info!("capture pour un agent : {}", request.path.display()),
            Err(e) => tracing::warn!("capture pour un agent impossible : {e}"),
        }
    }
    // Partage d'écran : images demandées par les clients (xdg-desktop-portal-wlr…)
    state.fulfill_screencast(
        &mut udev.renderer,
        &udev.shaders,
        &output,
        &elements,
        &elements[scene_start..],
    );
    let request = state.capture_request.take();
    if let Some(request) = request {
        let to_px = |r: smithay::utils::Rectangle<f64, smithay::utils::Logical>| {
            r.to_physical(scale).to_i32_round()
        };
        let result = match request {
            crate::screenshot::Request::Full => crate::capture::save(
                &mut udev.renderer,
                &elements[scene_start..],
                mode.size,
                scale,
                None,
                false,
            ),
            crate::screenshot::Request::Area(rect) => crate::capture::save(
                &mut udev.renderer,
                &elements[scene_start..],
                mode.size,
                scale,
                Some(to_px(smithay::utils::Rectangle::new(
                    rect.to_f64().loc - output_loc.to_f64(),
                    rect.to_f64().size,
                ))),
                false,
            ),
            crate::screenshot::Request::Window(window) => {
                let loc = state.space.element_location(&window).unwrap_or_default();
                let only = crate::render::single_window_elements(
                    &window,
                    loc,
                    &output,
                    &mut udev.renderer,
                    &udev.shaders,
                    mode.size,
                );
                // L'ombre comprise, comme sur macOS
                let m = theme::SHADOW_BLUR as f64;
                let geo = window.geometry().size.to_f64();
                let area = smithay::utils::Rectangle::new(
                    (loc.to_f64() - output_loc.to_f64()) - smithay::utils::Point::from((m, m)),
                    (
                        geo.w + 2.0 * m,
                        geo.h + 2.0 * m + theme::SHADOW_OFFSET_Y as f64,
                    )
                        .into(),
                );
                crate::capture::save(
                    &mut udev.renderer,
                    &only,
                    mode.size,
                    scale,
                    Some(to_px(area)),
                    true,
                )
            }
        };
        match result {
            Ok((path, png)) => state.capture_done(path, png),
            Err(e) => tracing::warn!("capture impossible : {e}"),
        }
    }

    let bg = if state.is_locked() {
        theme::Rgba::hex(0x000000, 1.0)
    } else {
        theme::palette().background
    };
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
