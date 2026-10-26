//! Clavier et pointeur virtuels du banc d'essai, tenus ouverts pendant toute
//! la session (protocoles wlr-virtual-pointer et zwp-virtual-keyboard) :
//!
//!     banc-entree --serveur [--ecran 1920x1200] [--disposition fr]
//!
//! branche les deux périphériques sur le compositeur Wayland et attend des
//! ordres sur $XDG_RUNTIME_DIR/banc-entree.sock. Chaque ordre est un autre
//! appel du même programme, qui revient quand c'est fait :
//!
//!     banc-entree texte 'firefox rame depuis ce matin'
//!     banc-entree touche ctrl+h Return BackSpace…
//!     banc-entree pointeur X Y        déplace le curseur
//!     banc-entree clic [X Y] [droit]  clique (du bouton gauche sinon)
//!     banc-entree glisser X1 Y1 X2 Y2 [PAS]
//!                                     appuie, glisse par PAS petits pas
//!                                     rapprochés (60 par défaut), relâche
//!     banc-entree molette N           tourne la molette de N crans (N < 0 : vers le haut)
//!     banc-entree ecran LxH           la taille de l'écran a changé
//!     banc-entree fin                 arrête le serveur
//!
//! Pourquoi pas wtype ? Il fabrique une disposition à lui, où les codes de
//! touche n'ont rien à voir avec un vrai clavier : cela convient à une appli
//! qui lit la disposition reçue, pas au compositeur odysseus imbriqué, qui
//! interprète les codes avec la sienne. Ici la disposition publiée est une
//! vraie (fr par défaut, la même que donne XKB_DEFAULT_LAYOUT au compositeur),
//! les codes sont ceux d'un clavier physique, et tout le monde lit pareil.
//! Et un périphérique qui reste : le survol tient, aucune frappe ne se perd
//! à l'apparition d'un clavier.

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::{
        fd::AsFd,
        unix::net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    time::{Duration, Instant},
};

use wayland_client::{
    Connection, Dispatch, QueueHandle, delegate_noop,
    protocol::{
        wl_pointer::{Axis, AxisSource, ButtonState},
        wl_registry, wl_seat,
    },
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1 as kb_manager, zwp_virtual_keyboard_v1 as kb,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1 as ptr_manager, zwlr_virtual_pointer_v1 as ptr,
};
use xkbcommon::xkb;

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
/// Entre deux touches : le temps qu'une appli en rendu logiciel suive
const KEY_PAUSE: Duration = Duration::from_millis(15);
const CLICK_PAUSE: Duration = Duration::from_millis(40);

fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").unwrap_or_else(|| "/tmp".into());
    PathBuf::from(dir).join("banc-entree.sock")
}

#[derive(Default)]
struct Globals {
    kb_manager: Option<kb_manager::ZwpVirtualKeyboardManagerV1>,
    ptr_manager: Option<ptr_manager::ZwlrVirtualPointerManagerV1>,
    seat: Option<wl_seat::WlSeat>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Globals {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "zwp_virtual_keyboard_manager_v1" => {
                    state.kb_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "zwlr_virtual_pointer_manager_v1" => {
                    state.ptr_manager = Some(registry.bind(name, version.min(2), qh, ()));
                }
                "wl_seat" => state.seat = Some(registry.bind(name, version.min(5), qh, ())),
                _ => {}
            }
        }
    }
}
delegate_noop!(Globals: ignore kb_manager::ZwpVirtualKeyboardManagerV1);
delegate_noop!(Globals: ignore kb::ZwpVirtualKeyboardV1);
delegate_noop!(Globals: ignore ptr_manager::ZwlrVirtualPointerManagerV1);
delegate_noop!(Globals: ignore ptr::ZwlrVirtualPointerV1);
delegate_noop!(Globals: ignore wl_seat::WlSeat);

/// Une touche à presser : son code (évdev) et les modificateurs qu'il lui faut
#[derive(Clone, Copy, Default)]
struct Stroke {
    code: u32,
    shift: bool,
    altgr: bool,
    ctrl: bool,
    alt: bool,
    logo: bool,
}

struct Server {
    conn: Connection,
    keyboard: kb::ZwpVirtualKeyboardV1,
    pointer: ptr::ZwlrVirtualPointerV1,
    keymap: xkb::Keymap,
    /// Fichier de la disposition : le compositeur le lit tant qu'on vit
    _keymap_file: fs::File,
    screen: (u32, u32),
    start: Instant,
}

impl Server {
    fn time(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    /// Code évdev de la première touche produisant ce symbole, et son niveau
    fn find(&self, sym: xkb::Keysym) -> Option<(u32, u32)> {
        let (min, max) = (
            self.keymap.min_keycode().raw(),
            self.keymap.max_keycode().raw(),
        );
        for raw in min..=max {
            let kc = xkb::Keycode::new(raw);
            for level in 0..self.keymap.num_levels_for_key(kc, 0) {
                if self
                    .keymap
                    .key_get_syms_by_level(kc, 0, level)
                    .contains(&sym)
                {
                    return Some((raw - 8, level));
                }
            }
        }
        None
    }

    fn modifier(&self, name: &str) -> Result<u32, String> {
        let sym = xkb::keysym_from_name(name, xkb::KEYSYM_NO_FLAGS);
        self.find(sym)
            .map(|(code, _)| code)
            .ok_or_else(|| format!("pas de touche {name} dans la disposition"))
    }

    fn stroke(&self, sym: xkb::Keysym, what: &str) -> Result<Stroke, String> {
        let (code, level) = self
            .find(sym)
            .ok_or_else(|| format!("« {what} » ne se tape pas avec cette disposition"))?;
        Ok(Stroke {
            code,
            shift: level & 1 == 1,
            altgr: level >= 2,
            ..Default::default()
        })
    }

    fn key(&self, code: u32, pressed: bool) {
        self.keyboard.key(self.time(), code, u32::from(pressed));
        let _ = self.conn.flush();
    }

    fn press(&self, s: Stroke) -> Result<(), String> {
        let mods = [
            (s.ctrl, "Control_L"),
            (s.alt, "Alt_L"),
            (s.logo, "Super_L"),
            (s.shift, "Shift_L"),
            (s.altgr, "ISO_Level3_Shift"),
        ];
        let mut held = Vec::new();
        for (on, name) in mods {
            if on {
                let code = self.modifier(name)?;
                self.key(code, true);
                held.push(code);
            }
        }
        std::thread::sleep(KEY_PAUSE / 3);
        self.key(s.code, true);
        std::thread::sleep(KEY_PAUSE / 3);
        self.key(s.code, false);
        for code in held.into_iter().rev() {
            self.key(code, false);
        }
        std::thread::sleep(KEY_PAUSE);
        Ok(())
    }

    fn text(&self, text: &str) -> Result<(), String> {
        for c in text.chars() {
            let sym = match c {
                '\n' => xkb::keysym_from_name("Return", xkb::KEYSYM_NO_FLAGS),
                '\t' => xkb::keysym_from_name("Tab", xkb::KEYSYM_NO_FLAGS),
                c => xkb::utf32_to_keysym(c as u32),
            };
            self.press(self.stroke(sym, &c.to_string())?)?;
        }
        Ok(())
    }

    /// « ctrl+maj+n », « Return », « super+space »…
    fn combo(&self, spec: &str) -> Result<(), String> {
        let mut parts: Vec<&str> = spec.split('+').collect();
        let key = parts.pop().filter(|k| !k.is_empty()).ok_or("touche vide")?;
        let mut sym = xkb::keysym_from_name(key, xkb::KEYSYM_NO_FLAGS);
        if sym == xkb::Keysym::NoSymbol {
            sym = xkb::keysym_from_name(key, xkb::KEYSYM_CASE_INSENSITIVE);
        }
        if sym == xkb::Keysym::NoSymbol {
            return Err(format!("touche inconnue : {key}"));
        }
        let mut s = self.stroke(sym, key)?;
        for m in parts {
            match m.to_lowercase().as_str() {
                "ctrl" | "control" => s.ctrl = true,
                "shift" | "maj" => s.shift = true,
                "alt" => s.alt = true,
                "altgr" => s.altgr = true,
                "super" | "logo" | "win" => s.logo = true,
                other => return Err(format!("modificateur inconnu : {other}")),
            }
        }
        self.press(s)
    }

    fn motion(&self, x: u32, y: u32) {
        self.pointer
            .motion_absolute(self.time(), x, y, self.screen.0, self.screen.1);
        self.pointer.frame();
        let _ = self.conn.flush();
    }

    /// Appui en (x1, y1), glissé jusqu'à (x2, y2) par `steps` pas espacés de
    /// 2 ms (une vraie souris envoie des centaines de mouvements par seconde),
    /// puis relâché
    fn drag(&self, (x1, y1): (u32, u32), (x2, y2): (u32, u32), steps: u32) {
        self.motion(x1, y1);
        std::thread::sleep(CLICK_PAUSE);
        self.pointer
            .button(self.time(), BTN_LEFT, ButtonState::Pressed);
        self.pointer.frame();
        let _ = self.conn.flush();
        let steps = steps.max(1);
        for i in 1..=steps {
            let lerp = |a: u32, b: u32| {
                (a as f64 + (b as f64 - a as f64) * i as f64 / steps as f64).round() as u32
            };
            self.motion(lerp(x1, x2), lerp(y1, y2));
            std::thread::sleep(Duration::from_millis(2));
        }
        std::thread::sleep(CLICK_PAUSE);
        self.pointer
            .button(self.time(), BTN_LEFT, ButtonState::Released);
        self.pointer.frame();
        let _ = self.conn.flush();
        std::thread::sleep(CLICK_PAUSE);
    }

    /// N crans de molette (15 unités chacun, comme une molette ordinaire)
    fn wheel(&self, notches: i32) {
        for _ in 0..notches.unsigned_abs() {
            let sign = notches.signum();
            self.pointer.axis_source(AxisSource::Wheel);
            self.pointer
                .axis_discrete(self.time(), Axis::VerticalScroll, 15.0 * sign as f64, sign);
            self.pointer.frame();
            let _ = self.conn.flush();
            std::thread::sleep(Duration::from_millis(8));
        }
        std::thread::sleep(CLICK_PAUSE);
    }

    fn click(&self, button: u32) {
        self.pointer
            .button(self.time(), button, ButtonState::Pressed);
        self.pointer.frame();
        let _ = self.conn.flush();
        std::thread::sleep(CLICK_PAUSE);
        self.pointer
            .button(self.time(), button, ButtonState::Released);
        self.pointer.frame();
        let _ = self.conn.flush();
        std::thread::sleep(CLICK_PAUSE);
    }

    /// Un ordre reçu ; Ok(false) : c'est la fin
    fn handle(&mut self, line: &str) -> Result<bool, String> {
        let (cmd, arg) = line.split_once('\t').unwrap_or((line, ""));
        let arg = unescape(arg);
        match cmd {
            "texte" => self.text(&arg)?,
            "touche" => {
                for spec in arg.split_whitespace() {
                    self.combo(spec)?;
                }
            }
            "pointeur" => {
                let (x, y) = coords(&arg)?;
                self.motion(x, y);
                std::thread::sleep(CLICK_PAUSE);
            }
            "clic" => {
                let mut words: Vec<&str> = arg.split_whitespace().collect();
                let button = match words.last().copied() {
                    Some("droit") => {
                        words.pop();
                        BTN_RIGHT
                    }
                    Some("milieu") => {
                        words.pop();
                        BTN_MIDDLE
                    }
                    _ => BTN_LEFT,
                };
                if !words.is_empty() {
                    let (x, y) = coords(&words.join(" "))?;
                    self.motion(x, y);
                    std::thread::sleep(CLICK_PAUSE);
                }
                self.click(button);
            }
            "glisser" => {
                let n: Vec<u32> = arg
                    .split_whitespace()
                    .map(|w| w.parse().map_err(|_| format!("nombre attendu : {w}")))
                    .collect::<Result<_, _>>()?;
                let [x1, y1, x2, y2] = n[..n.len().min(4)] else {
                    return Err("glisser X1 Y1 X2 Y2 [PAS]".into());
                };
                self.drag((x1, y1), (x2, y2), n.get(4).copied().unwrap_or(60));
            }
            "molette" => {
                let n: i32 = arg.trim().parse().map_err(|_| "molette N".to_string())?;
                self.wheel(n);
            }
            "ecran" => self.screen = screen_size(&arg)?,
            "fin" => return Ok(false),
            other => return Err(format!("ordre inconnu : {other}")),
        }
        Ok(true)
    }
}

/// « 1920x1200 »
fn screen_size(arg: &str) -> Result<(u32, u32), String> {
    arg.trim()
        .split_once('x')
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .ok_or_else(|| format!("taille d'écran attendue : LxH, reçu « {arg} »"))
}

fn coords(arg: &str) -> Result<(u32, u32), String> {
    let mut it = arg.split_whitespace().map(|n| n.parse::<u32>());
    match (it.next(), it.next()) {
        (Some(Ok(x)), Some(Ok(y))) => Ok((x, y)),
        _ => Err(format!("coordonnées attendues : X Y, reçu « {arg} »")),
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

fn serve(screen: (u32, u32), layout: &str) -> Result<(), String> {
    let conn = Connection::connect_to_env()
        .map_err(|e| format!("pas de compositeur Wayland (WAYLAND_DISPLAY) : {e}"))?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut globals = Globals::default();
    queue.roundtrip(&mut globals).map_err(|e| e.to_string())?;
    let seat = globals.seat.take().ok_or("pas de wl_seat")?;
    let kb_manager = globals
        .kb_manager
        .take()
        .ok_or("le compositeur n'offre pas zwp_virtual_keyboard_manager_v1")?;
    let ptr_manager = globals
        .ptr_manager
        .take()
        .ok_or("le compositeur n'offre pas zwlr_virtual_pointer_manager_v1")?;

    // La disposition, compilée comme le ferait le compositeur, publiée telle quelle
    let (layout, variant) = layout.split_once(':').unwrap_or((layout, ""));
    let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap = xkb::Keymap::new_from_names(
        &ctx,
        "",
        "",
        layout,
        variant,
        None,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .ok_or_else(|| format!("disposition inconnue : {layout}"))?;
    let mut text = keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);
    text.push('\0');
    let keymap_path = socket_path().with_extension("keymap");
    fs::write(&keymap_path, &text).map_err(|e| e.to_string())?;
    let keymap_file = fs::File::open(&keymap_path).map_err(|e| e.to_string())?;
    let keyboard = kb_manager.create_virtual_keyboard(&seat, &qh, ());
    keyboard.keymap(1, keymap_file.as_fd(), text.len() as u32);
    let pointer = ptr_manager.create_virtual_pointer(Some(&seat), &qh, ());
    conn.flush().map_err(|e| e.to_string())?;

    let sock = socket_path();
    let _ = fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).map_err(|e| format!("{} : {e}", sock.display()))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let mut server = Server {
        conn,
        keyboard,
        pointer,
        keymap,
        _keymap_file: keymap_file,
        screen,
        start: Instant::now(),
    };
    // Le curseur existe dès le départ, dans un coin
    server.motion(screen.0 - 2, screen.1 - 2);
    println!(
        "banc-entree : clavier {layout}{variant}, écran {}x{}, {}",
        screen.0,
        screen.1,
        sock.display()
    );

    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let mut line = String::new();
                let _ = BufReader::new(&stream).read_line(&mut line);
                let line = line.trim_end_matches('\n');
                let reply = match server.handle(line) {
                    Ok(true) => "ok\n".to_string(),
                    Ok(false) => {
                        let _ = stream.write_all(b"ok\n");
                        break;
                    }
                    Err(e) => format!("erreur : {e}\n"),
                };
                let _ = stream.write_all(reply.as_bytes());
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Rien à faire : on lit ce que le compositeur a pu envoyer
                let _ = queue.roundtrip(&mut globals);
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    let _ = fs::remove_file(&sock);
    let _ = fs::remove_file(&keymap_path);
    Ok(())
}

fn send(cmd: &str, arg: &str) -> Result<(), String> {
    let sock = socket_path();
    let mut stream = UnixStream::connect(&sock).map_err(|e| {
        format!(
            "banc-entree --serveur ne tourne pas ({}) : {e}",
            sock.display()
        )
    })?;
    stream
        .write_all(format!("{cmd}\t{}\n", escape(arg)).as_bytes())
        .map_err(|e| e.to_string())?;
    let mut reply = String::new();
    stream
        .read_to_string(&mut reply)
        .map_err(|e| e.to_string())?;
    match reply.trim() {
        "ok" => Ok(()),
        other => Err(other.trim_start_matches("erreur : ").to_string()),
    }
}

fn usage() -> ! {
    eprintln!("usage : banc-entree --serveur [--ecran LxH] [--disposition fr]");
    eprintln!(
        "        banc-entree texte TEXTE | touche TOUCHE… | pointeur X Y | clic [X Y] [droit]\n        banc-entree glisser X1 Y1 X2 Y2 [PAS] | molette N | ecran LxH | fin"
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("--serveur") => {
            let mut screen = (1920, 1200);
            let mut layout = std::env::var("BANC_DISPOSITION").unwrap_or_else(|_| "fr".into());
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--ecran" => {
                        let v = it.next().unwrap_or_else(|| usage());
                        screen = screen_size(v).unwrap_or_else(|_| usage());
                    }
                    "--disposition" => layout = it.next().unwrap_or_else(|| usage()).clone(),
                    _ => usage(),
                }
            }
            serve(screen, &layout)
        }
        Some(
            cmd @ ("texte" | "touche" | "pointeur" | "clic" | "glisser" | "molette" | "ecran"
            | "fin"),
        ) => send(cmd, &args[1..].join(" ")),
        _ => usage(),
    };
    if let Err(e) = result {
        eprintln!("banc-entree : {e}");
        std::process::exit(1);
    }
}
