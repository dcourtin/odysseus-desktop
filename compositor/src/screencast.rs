//! Partage d'écran : protocoles ext-image-copy-capture-v1 (copie d'images),
//! ext-image-capture-source-v1 (source : un écran ou une fenêtre) et
//! ext-foreign-toplevel-list-v1 (liste des fenêtres partageables).
//!
//! xdg-desktop-portal-wlr s'en sert pour le portail ScreenCast (Firefox,
//! visioconférence…) ; grim aussi. Une image n'est copiée que si l'écran (ou
//! la fenêtre) a changé depuis la précédente, au rythme du rendu. Rien n'est
//! copié pendant le verrouillage. Tant qu'une session est ouverte, la barre
//! affiche un témoin « partage en cours » (IPC GET_SCREENCAST).

use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::UNIX_EPOCH,
};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, ExportMem, Offscreen, TextureMapping,
            damage::OutputDamageTracker,
            gles::{GlesRenderer, GlesTexture},
        },
    },
    desktop::Window,
    output::Output,
    reexports::{
        wayland_protocols::ext::{
            image_capture_source::v1::server::{
                ext_foreign_toplevel_image_capture_source_manager_v1::{
                    self, ExtForeignToplevelImageCaptureSourceManagerV1,
                },
                ext_image_capture_source_v1::{self, ExtImageCaptureSourceV1},
                ext_output_image_capture_source_manager_v1::{
                    self, ExtOutputImageCaptureSourceManagerV1,
                },
            },
            image_copy_capture::v1::server::{
                ext_image_copy_capture_cursor_session_v1::{
                    self, ExtImageCopyCaptureCursorSessionV1,
                },
                ext_image_copy_capture_frame_v1::{
                    self, ExtImageCopyCaptureFrameV1, FailureReason,
                },
                ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1, Options},
                ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
            },
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
            backend::ObjectId,
            protocol::{wl_buffer::WlBuffer, wl_output, wl_shm},
        },
    },
    utils::{Buffer as BufferCoords, Logical, Physical, Point, Rectangle, Size, Transform},
    wayland::{
        foreign_toplevel_list::{
            ForeignToplevelHandle, ForeignToplevelListHandler, ForeignToplevelListState,
        },
        shm,
    },
};

use crate::{Odysseus, render::OdysseusElement};

/// Ce qu'on partage
#[derive(Clone)]
pub enum Source {
    Output(Output),
    /// Fenêtre, par son identifiant odysseus (agent::window_id)
    Window(u64),
    /// Source devenue invalide (fenêtre déjà fermée…)
    Gone,
}

/// Session de capture : source, curseur dessiné ou non, taille annoncée
pub struct SessionData {
    source: Source,
    cursors: bool,
    stopped: AtomicBool,
    /// Taille de tampon annoncée au client (pixels)
    size: Mutex<Size<i32, Physical>>,
    has_frame: AtomicBool,
}

pub struct FrameData {
    session: ExtImageCopyCaptureSessionV1,
    buffer: Mutex<Option<WlBuffer>>,
    captured: AtomicBool,
}

/// Rendu propre à une session : texture réutilisée, d'où un rendu limité aux dégâts
struct SessionRender {
    texture: GlesTexture,
    tracker: OutputDamageTracker,
    size: Size<i32, Physical>,
    age: usize,
}

pub struct ScreencastState {
    pub toplevel_list: ForeignToplevelListState,
    /// Fenêtres publiées dans ext-foreign-toplevel-list, par identifiant odysseus
    toplevels: HashMap<u64, ForeignToplevelHandle>,
    sessions: Vec<ExtImageCopyCaptureSessionV1>,
    /// Images demandées, copiées au prochain rendu où la source a changé
    pending: Vec<ExtImageCopyCaptureFrameV1>,
    renders: HashMap<ObjectId, SessionRender>,
    /// Nombre de sessions au dernier signalement (témoin de la barre)
    pub announced: usize,
}

/// Identifiant odysseus d'une fenêtre publiée (données du ForeignToplevelHandle)
struct WindowRef(u64);

/// Formats proposés : RGBA (lecture directe de la texture), puis BGRA
/// (le plus courant ; octets permutés à la copie)
const FORMATS: [wl_shm::Format; 4] = [
    wl_shm::Format::Xbgr8888,
    wl_shm::Format::Abgr8888,
    wl_shm::Format::Xrgb8888,
    wl_shm::Format::Argb8888,
];

impl ScreencastState {
    pub fn new(dh: &DisplayHandle) -> Self {
        dh.create_global::<Odysseus, ExtOutputImageCaptureSourceManagerV1, _>(1, ());
        dh.create_global::<Odysseus, ExtForeignToplevelImageCaptureSourceManagerV1, _>(1, ());
        dh.create_global::<Odysseus, ExtImageCopyCaptureManagerV1, _>(1, ());
        Self {
            toplevel_list: ForeignToplevelListState::new::<Odysseus>(dh),
            toplevels: HashMap::new(),
            sessions: Vec::new(),
            pending: Vec::new(),
            renders: HashMap::new(),
            announced: 0,
        }
    }

    /// Sessions de partage en cours (hors sessions déjà arrêtées)
    pub fn active(&self) -> usize {
        self.sessions
            .iter()
            .filter(|s| {
                s.data::<SessionData>()
                    .is_some_and(|d| !d.stopped.load(Ordering::Relaxed))
            })
            .count()
    }

    /// Identifiant ext-foreign-toplevel d'une fenêtre (sélecteur de partage)
    pub fn identifier(&self, window_id: u64) -> Option<String> {
        self.toplevels.get(&window_id).map(|h| h.identifier())
    }
}

fn session_data(session: &ExtImageCopyCaptureSessionV1) -> Option<&SessionData> {
    session.data::<SessionData>()
}

impl Odysseus {
    /// Liste des fenêtres publiée aux clients : ajouts, titres, fermetures
    pub fn sync_toplevels(&mut self) {
        let mut current: Vec<(u64, String, String)> = Vec::new();
        for window in self.space.elements() {
            let (app, title) = crate::agent::app_and_title(window);
            current.push((crate::agent::window_id(window), app, title));
        }
        for ws in 0..crate::workspaces::COUNT {
            for (window, _) in self.workspaces.hidden(ws) {
                let (app, title) = crate::agent::app_and_title(window);
                current.push((crate::agent::window_id(window), app, title));
            }
        }
        let cast = &mut self.screencast;
        // Fermées
        let gone: Vec<u64> = cast
            .toplevels
            .keys()
            .filter(|id| !current.iter().any(|(c, _, _)| c == *id))
            .copied()
            .collect();
        for id in gone {
            if let Some(handle) = cast.toplevels.remove(&id) {
                cast.toplevel_list.remove_toplevel(&handle);
            }
            // Partage de cette fenêtre : terminé
            for session in &cast.sessions {
                if let Some(d) = session_data(session)
                    && matches!(d.source, Source::Window(w) if w == id)
                    && !d.stopped.swap(true, Ordering::Relaxed)
                {
                    session.stopped();
                }
            }
        }
        for (id, app, title) in current {
            match cast.toplevels.get(&id) {
                Some(handle) => {
                    let mut changed = false;
                    if handle.title() != title {
                        handle.send_title(&title);
                        changed = true;
                    }
                    if handle.app_id() != app {
                        handle.send_app_id(&app);
                        changed = true;
                    }
                    if changed {
                        handle.send_done();
                    }
                }
                None => {
                    let handle = cast.toplevel_list.new_toplevel::<Odysseus>(&title, &app);
                    handle
                        .user_data()
                        .insert_if_missing_threadsafe(|| WindowRef(id));
                    cast.toplevels.insert(id, handle);
                }
            }
        }
    }

    /// Arrête tous les partages (bouton du témoin dans la barre)
    pub fn stop_screencasts(&mut self) {
        for session in &self.screencast.sessions {
            if let Some(d) = session_data(session)
                && !d.stopped.swap(true, Ordering::Relaxed)
            {
                session.stopped();
            }
        }
        for frame in std::mem::take(&mut self.screencast.pending) {
            frame.failed(FailureReason::Stopped);
        }
        self.screencast_changed();
    }

    /// Le témoin de la barre suit le nombre de sessions
    pub fn screencast_changed(&mut self) {
        let n = self.screencast.active();
        if n != self.screencast.announced {
            self.screencast.announced = n;
            crate::ipc::broadcast_screencast(self, n);
        }
    }

    /// Taille du tampon pour une source, en pixels
    fn source_size(&self, source: &Source) -> Option<Size<i32, Physical>> {
        match source {
            Source::Output(output) => output.current_mode().map(|m| m.size),
            Source::Window(id) => {
                let (window, _) = self.window_by_id(*id)?;
                let scale = self
                    .space
                    .outputs()
                    .next()
                    .map(|o| o.current_scale().fractional_scale())
                    .unwrap_or(1.0);
                let size = window
                    .geometry()
                    .size
                    .to_f64()
                    .to_physical(scale)
                    .to_i32_round();
                (size.w > 0 && size.h > 0).then_some(size)
            }
            Source::Gone => None,
        }
    }

    /// Annonce (ou ré-annonce) les contraintes de tampon d'une session
    fn send_constraints(&self, session: &ExtImageCopyCaptureSessionV1) -> bool {
        let Some(d) = session_data(session) else {
            return false;
        };
        let Some(size) = self.source_size(&d.source) else {
            return false;
        };
        *d.size.lock().unwrap() = size;
        session.buffer_size(size.w as u32, size.h as u32);
        for f in FORMATS {
            session.shm_format(f);
        }
        session.done();
        true
    }

    /// Après le rendu d'un écran : copie des images en attente dont la source
    /// est cet écran (ou une fenêtre). `all` : la scène avec curseur et
    /// couche de capture ; `scene` : la scène seule.
    pub fn fulfill_screencast(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &crate::render::Shaders,
        output: &Output,
        all: &[OdysseusElement],
        scene: &[OdysseusElement],
    ) {
        if self.screencast.pending.is_empty() {
            return;
        }
        let locked = self.is_locked();
        let scale = output.current_scale().fractional_scale();
        let pending = std::mem::take(&mut self.screencast.pending);
        let mut keep = Vec::new();
        for frame in pending {
            let Some(fd) = frame.data::<FrameData>() else {
                continue;
            };
            let session = fd.session.clone();
            let Some(sd) = session_data(&session) else {
                continue;
            };
            if sd.stopped.load(Ordering::Relaxed) {
                frame.failed(FailureReason::Stopped);
                continue;
            }
            // Verrouillé : on attend le déverrouillage, rien ne sort
            if locked {
                keep.push(frame);
                continue;
            }
            // La taille a changé (écran, fenêtre redimensionnée) : nouvelles contraintes
            let wanted = *sd.size.lock().unwrap();
            let size = self.source_size(&sd.source);
            if size != Some(wanted) {
                frame.failed(FailureReason::BufferConstraints);
                self.screencast.renders.remove(&session.id());
                if !self.send_constraints(&session) && !sd.stopped.swap(true, Ordering::Relaxed) {
                    session.stopped();
                }
                continue;
            }
            let window_elements;
            let elements: &[OdysseusElement] = match &sd.source {
                Source::Output(o) if o == output => {
                    if sd.cursors {
                        all
                    } else {
                        scene
                    }
                }
                Source::Output(_) => {
                    keep.push(frame);
                    continue;
                }
                Source::Window(id) => {
                    let Some((window, _)) = self.window_by_id(*id) else {
                        frame.failed(FailureReason::Stopped);
                        continue;
                    };
                    window_elements = single_window(&window, output, renderer, shaders, wanted);
                    &window_elements
                }
                Source::Gone => {
                    frame.failed(FailureReason::Stopped);
                    continue;
                }
            };
            let first = !sd.has_frame.load(Ordering::Relaxed);
            let transparent = matches!(sd.source, Source::Window(_));
            match copy_frame(
                &mut self.screencast.renders,
                &session,
                &frame,
                renderer,
                elements,
                wanted,
                scale,
                transparent,
                first,
            ) {
                Ok(true) => sd.has_frame.store(true, Ordering::Relaxed),
                // Rien n'a changé : l'image attend le prochain changement
                Ok(false) => keep.push(frame),
                Err(e) => {
                    tracing::warn!("partage d'écran : {e}");
                    frame.failed(FailureReason::Unknown);
                }
            }
        }
        self.screencast.pending = keep;
    }
}

/// Fenêtre seule, coin haut gauche de sa géométrie en (0, 0) (la position
/// d'un élément de l'espace est déjà celle de sa géométrie)
fn single_window(
    window: &Window,
    output: &Output,
    renderer: &mut GlesRenderer,
    shaders: &crate::render::Shaders,
    size: Size<i32, Physical>,
) -> Vec<OdysseusElement> {
    crate::render::single_window_elements(
        window,
        Point::<i32, Logical>::default(),
        output,
        renderer,
        shaders,
        size,
    )
}

/// Rend la source dans la texture de la session puis la copie dans le
/// tampon du client. Ok(false) : aucun changement, rien de copié.
#[allow(clippy::too_many_arguments)]
fn copy_frame(
    renders: &mut HashMap<ObjectId, SessionRender>,
    session: &ExtImageCopyCaptureSessionV1,
    frame: &ExtImageCopyCaptureFrameV1,
    renderer: &mut GlesRenderer,
    elements: &[OdysseusElement],
    size: Size<i32, Physical>,
    scale: f64,
    transparent: bool,
    first: bool,
) -> Result<bool, String> {
    let fd = frame.data::<FrameData>().ok_or("image sans données")?;
    let buffer = fd
        .buffer
        .lock()
        .unwrap()
        .clone()
        .ok_or("image sans tampon")?;
    let buffer_size: Size<i32, BufferCoords> = (size.w, size.h).into();
    if !renders.get(&session.id()).is_some_and(|r| r.size == size) {
        let texture: GlesTexture = renderer
            .create_buffer(Fourcc::Abgr8888, buffer_size)
            .map_err(|e| format!("texture : {e}"))?;
        renders.insert(
            session.id(),
            SessionRender {
                texture,
                tracker: OutputDamageTracker::new(size, scale, Transform::Normal),
                size,
                age: 0,
            },
        );
    }
    let r = renders.get_mut(&session.id()).unwrap();
    let clear = if transparent {
        [0.0, 0.0, 0.0, 0.0]
    } else {
        let bg = common::theme::palette().background;
        [bg.r, bg.g, bg.b, 1.0]
    };
    let (damage, mapping) = {
        let mut fb = renderer
            .bind(&mut r.texture)
            .map_err(|e| format!("texture : {e}"))?;
        let result = r
            .tracker
            .render_output(renderer, &mut fb, r.age, elements, clear)
            .map_err(|e| format!("rendu : {e:?}"))?;
        let damage: Vec<Rectangle<i32, Physical>> = result.damage.cloned().unwrap_or_default();
        r.age = 1;
        if damage.is_empty() && !first {
            return Ok(false);
        }
        let mapping = renderer
            .copy_framebuffer(&fb, Rectangle::from_size(buffer_size), Fourcc::Abgr8888)
            .map_err(|e| format!("lecture : {e}"))?;
        (damage, mapping)
    };
    let flipped = mapping.flipped();
    let pixels = renderer
        .map_texture(&mapping)
        .map_err(|e| format!("lecture : {e}"))?;
    let stride = size.w as usize * 4;
    shm::with_buffer_contents_mut(&buffer, |ptr, len, data| {
        let swap = matches!(
            data.format,
            wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888
        );
        let dst_stride = data.stride as usize;
        let rows = size.h as usize;
        for y in 0..rows {
            // glReadPixels : lignes de bas en haut, sauf texture déjà retournée
            let src_y = if flipped { y } else { rows - 1 - y };
            let src = &pixels[src_y * stride..(src_y + 1) * stride];
            let start = data.offset as usize + y * dst_stride;
            if start + stride > len {
                break;
            }
            let dst = unsafe { std::slice::from_raw_parts_mut(ptr.add(start), stride) };
            if swap {
                for (d, s) in dst
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(src.as_chunks::<4>().0)
                {
                    d[0] = s[2];
                    d[1] = s[1];
                    d[2] = s[0];
                    d[3] = s[3];
                }
            } else {
                dst.copy_from_slice(src);
            }
        }
    })
    .map_err(|e| format!("tampon : {e:?}"))?;

    frame.transform(wl_output::Transform::Normal);
    if first || damage.is_empty() {
        frame.damage(0, 0, size.w, size.h);
    } else {
        for d in damage {
            frame.damage(d.loc.x, d.loc.y, d.size.w, d.size.h);
        }
    }
    let now = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    frame.presentation_time((secs >> 32) as u32, secs as u32, now.subsec_nanos());
    frame.ready();
    fd.captured.store(true, Ordering::Relaxed);
    Ok(true)
}

/// Tampon conforme aux contraintes annoncées (shm, taille, format)
fn buffer_ok(buffer: &WlBuffer, size: Size<i32, Physical>) -> bool {
    shm::with_buffer_contents(buffer, |_, len, d| {
        d.width == size.w
            && d.height == size.h
            && d.stride >= size.w * 4
            && FORMATS.contains(&d.format)
            && (d.offset as usize + d.stride as usize * d.height as usize) <= len
    })
    .unwrap_or(false)
}

// --- Liste des fenêtres --------------------------------------------------------------

impl ForeignToplevelListHandler for Odysseus {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        &mut self.screencast.toplevel_list
    }
}
smithay::delegate_foreign_toplevel_list!(Odysseus);

// --- Sources -------------------------------------------------------------------------

impl GlobalDispatch<ExtOutputImageCaptureSourceManagerV1, ()> for Odysseus {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<ExtOutputImageCaptureSourceManagerV1>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<ExtOutputImageCaptureSourceManagerV1, ()> for Odysseus {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ExtOutputImageCaptureSourceManagerV1,
        request: ext_output_image_capture_source_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let ext_output_image_capture_source_manager_v1::Request::CreateSource {
            source,
            output,
        } = request
        {
            let src = Output::from_resource(&output)
                .map(Source::Output)
                .unwrap_or(Source::Gone);
            init.init(source, Mutex::new(src));
        }
    }
}

impl GlobalDispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ()> for Odysseus {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<ExtForeignToplevelImageCaptureSourceManagerV1>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ()> for Odysseus {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ExtForeignToplevelImageCaptureSourceManagerV1,
        request: ext_foreign_toplevel_image_capture_source_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let ext_foreign_toplevel_image_capture_source_manager_v1::Request::CreateSource {
            source,
            toplevel_handle,
        } = request
        {
            let src = ForeignToplevelHandle::from_resource(&toplevel_handle)
                .filter(|h| !h.is_closed())
                .and_then(|h| h.user_data().get::<WindowRef>().map(|w| w.0))
                .map(Source::Window)
                .unwrap_or(Source::Gone);
            init.init(source, Mutex::new(src));
        }
    }
}

impl Dispatch<ExtImageCaptureSourceV1, Mutex<Source>> for Odysseus {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ExtImageCaptureSourceV1,
        _: ext_image_capture_source_v1::Request,
        _: &Mutex<Source>,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

// --- Copie ---------------------------------------------------------------------------

impl GlobalDispatch<ExtImageCopyCaptureManagerV1, ()> for Odysseus {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<ExtImageCopyCaptureManagerV1>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<ExtImageCopyCaptureManagerV1, ()> for Odysseus {
    fn request(
        state: &mut Self,
        _: &Client,
        manager: &ExtImageCopyCaptureManagerV1,
        request: ext_image_copy_capture_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_image_copy_capture_manager_v1::Request::CreateSession {
                session,
                source,
                options,
            } => {
                let cursors = match options.into_result() {
                    Ok(o) => o.contains(Options::PaintCursors),
                    Err(_) => {
                        manager.post_error(
                            ext_image_copy_capture_manager_v1::Error::InvalidOption,
                            "option inconnue",
                        );
                        return;
                    }
                };
                let src = source
                    .data::<Mutex<Source>>()
                    .map(|s| s.lock().unwrap().clone())
                    .unwrap_or(Source::Gone);
                let session = init.init(
                    session,
                    SessionData {
                        source: src,
                        cursors,
                        stopped: AtomicBool::new(false),
                        size: Mutex::new(Size::default()),
                        has_frame: AtomicBool::new(false),
                    },
                );
                if state.send_constraints(&session) {
                    tracing::info!("partage d'écran : nouvelle session");
                } else if let Some(d) = session_data(&session) {
                    d.stopped.store(true, Ordering::Relaxed);
                    session.stopped();
                }
                state.screencast.sessions.push(session);
                state.screencast_changed();
            }
            ext_image_copy_capture_manager_v1::Request::CreatePointerCursorSession {
                session,
                ..
            } => {
                // Pas de capture du curseur à part : il est dessiné dans l'image
                init.init(session, AtomicBool::new(false));
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureCursorSessionV1, AtomicBool> for Odysseus {
    fn request(
        _: &mut Self,
        _: &Client,
        resource: &ExtImageCopyCaptureCursorSessionV1,
        request: ext_image_copy_capture_cursor_session_v1::Request,
        used: &AtomicBool,
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let ext_image_copy_capture_cursor_session_v1::Request::GetCaptureSession { session } =
            request
        {
            if used.swap(true, Ordering::Relaxed) {
                resource.post_error(
                    ext_image_copy_capture_cursor_session_v1::Error::DuplicateSession,
                    "session déjà demandée",
                );
                return;
            }
            let session = init.init(
                session,
                SessionData {
                    source: Source::Gone,
                    cursors: false,
                    stopped: AtomicBool::new(true),
                    size: Mutex::new(Size::default()),
                    has_frame: AtomicBool::new(false),
                },
            );
            session.stopped();
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, SessionData> for Odysseus {
    fn request(
        state: &mut Self,
        _: &Client,
        session: &ExtImageCopyCaptureSessionV1,
        request: ext_image_copy_capture_session_v1::Request,
        _: &SessionData,
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_image_copy_capture_session_v1::Request::CreateFrame { frame } => {
                let busy = state.screencast.pending.iter().any(|f| {
                    f.data::<FrameData>().is_some_and(|d| {
                        d.session == *session && !d.captured.load(Ordering::Relaxed)
                    })
                });
                init.init(
                    frame,
                    FrameData {
                        session: session.clone(),
                        buffer: Mutex::new(None),
                        captured: AtomicBool::new(false),
                    },
                );
                if busy {
                    session.post_error(
                        ext_image_copy_capture_session_v1::Error::DuplicateFrame,
                        "une image est déjà en cours",
                    );
                }
            }
            ext_image_copy_capture_session_v1::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _: smithay::reexports::wayland_server::backend::ClientId,
        session: &ExtImageCopyCaptureSessionV1,
        _: &SessionData,
    ) {
        state.screencast.sessions.retain(|s| s != session);
        state.screencast.renders.remove(&session.id());
        state
            .screencast
            .pending
            .retain(|f| f.data::<FrameData>().is_some_and(|d| d.session != *session));
        tracing::info!("partage d'écran : session terminée");
        state.screencast_changed();
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, FrameData> for Odysseus {
    fn request(
        state: &mut Self,
        _: &Client,
        frame: &ExtImageCopyCaptureFrameV1,
        request: ext_image_copy_capture_frame_v1::Request,
        data: &FrameData,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_image_copy_capture_frame_v1::Request::AttachBuffer { buffer } => {
                *data.buffer.lock().unwrap() = Some(buffer);
            }
            ext_image_copy_capture_frame_v1::Request::DamageBuffer {
                x,
                y,
                width,
                height,
            } => {
                if x < 0 || y < 0 || width <= 0 || height <= 0 {
                    frame.post_error(
                        ext_image_copy_capture_frame_v1::Error::InvalidBufferDamage,
                        "zone invalide",
                    );
                }
            }
            ext_image_copy_capture_frame_v1::Request::Capture => {
                if data.captured.load(Ordering::Relaxed) || state.screencast.pending.contains(frame)
                {
                    frame.post_error(
                        ext_image_copy_capture_frame_v1::Error::AlreadyCaptured,
                        "image déjà demandée",
                    );
                    return;
                }
                let Some(buffer) = data.buffer.lock().unwrap().clone() else {
                    frame.post_error(
                        ext_image_copy_capture_frame_v1::Error::NoBuffer,
                        "aucun tampon",
                    );
                    return;
                };
                let Some(sd) = session_data(&data.session) else {
                    frame.failed(FailureReason::Unknown);
                    return;
                };
                if sd.stopped.load(Ordering::Relaxed) {
                    frame.failed(FailureReason::Stopped);
                    return;
                }
                if !buffer_ok(&buffer, *sd.size.lock().unwrap()) {
                    frame.failed(FailureReason::BufferConstraints);
                    return;
                }
                state.screencast.pending.push(frame.clone());
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _: smithay::reexports::wayland_server::backend::ClientId,
        frame: &ExtImageCopyCaptureFrameV1,
        _: &FrameData,
    ) {
        state.screencast.pending.retain(|f| f != frame);
    }
}
