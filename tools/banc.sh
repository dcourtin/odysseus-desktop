#!/bin/bash
# Banc d'essai d'odysseus : les applis du bureau tournent dans un sway
# « headless » (sans écran ni carte graphique), avec un HOME de démonstration,
# un clavier et un pointeur virtuels, et grim capture chaque vue. De quoi
# vérifier à l'œil, sans machine sous la main, et avant de pousser.
#
#   tools/banc.sh [--scene NOM]… [--garder] [--sans-compiler] [--compositeur]
#
#   --scene NOM      ne joue que cette scène (répétable) ; sans : toutes
#                    assistant · reglages · notifications · fichiers · compositeur
#   --garder         laisse la session tourner à la fin (puis : source target/banc/env.sh)
#   --sans-compiler  ne recompile pas les binaires
#   --compositeur    ajoute la scène du compositeur odysseus imbriqué (rendu logiciel)
#
# Dépendances (Debian, Ubuntu) : sway grim wl-clipboard dbus. Le clavier et le
# pointeur sont ceux de banc-entree (tools/banc), compilés avec le reste.
# Résultat : target/banc/captures/*.png. Le HOME du banc est target/banc/home :
# rien n'est lu ni écrit chez vous. BANC_DISPOSITION=us change la disposition
# du clavier virtuel (fr par défaut), partagée avec le compositeur imbriqué.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
BANC=$ROOT/target/banc
CAPTURES=$BANC/captures
# Les sockets Unix veulent un chemin court : hors du dépôt
RUN=/tmp/odysseus-banc-$(id -u)
BIN=$ROOT/target/debug
W=1920
H=1200
DISPOSITION=${BANC_DISPOSITION:-fr}

scenes=()
garder=0
compiler=1
compositeur=0
while [ $# -gt 0 ]; do
    case $1 in
        --scene) scenes+=("$2"); shift ;;
        --garder) garder=1 ;;
        --sans-compiler) compiler=0 ;;
        --compositeur) compositeur=1 ;;
        -h|--help) sed -n '2,19p' "$0"; exit 0 ;;
        *) echo "banc.sh : option inconnue $1" >&2; exit 2 ;;
    esac
    shift
done
if [ ${#scenes[@]} -eq 0 ]; then
    scenes=(assistant reglages notifications fichiers)
    [ $compositeur -eq 1 ] && scenes+=(compositeur)
fi

say() { printf '\033[1m%s\033[0m\n' "$*"; }

# --- Dépendances et compilation -------------------------------------------------
for tool in sway grim wl-copy dbus-daemon; do
    command -v "$tool" >/dev/null || { echo "banc.sh : $tool manquant (apt install sway grim wl-clipboard dbus)" >&2; exit 1; }
done
if [ $compiler -eq 1 ]; then
    say "Compilation"
    crates=(-p banc -p assistant -p settings -p notify -p files -p notes)
    [ $compositeur -eq 1 ] && crates+=(-p compositor)
    (cd "$ROOT" && cargo build --quiet "${crates[@]}")
fi
binaires=(banc-entree banc-donnees odysseus-assistant odysseus-reglages odysseus-notify files odysseus-notes)
[ $compositeur -eq 1 ] && binaires+=(compositor)
for b in "${binaires[@]}"; do
    [ -x "$BIN/$b" ] || { echo "banc.sh : $BIN/$b manquant (relancez sans --sans-compiler)" >&2; exit 1; }
done

# --- La session ------------------------------------------------------------------
say "Session sway headless"
rm -rf "$BANC/home"
mkdir -p "$BANC/home" "$CAPTURES"
mkdir -p "$RUN" && chmod 700 "$RUN"
rm -f "$RUN"/wayland-* "$RUN"/sway.sock "$RUN"/bus "$RUN"/*.pid "$RUN"/odysseus-*.sock "$RUN"/odysseus-*.json "$RUN"/banc-entree.*
cat > "$BANC/sway.conf" <<EOF
output HEADLESS-1 resolution ${W}x${H} position 0,0 scale 1
output * bg #1b2838 solid_color
default_border none
focus_follows_mouse no
EOF
cat > "$BANC/env.sh" <<EOF
export HOME=$BANC/home
export XDG_CONFIG_HOME=$BANC/home/.config
export XDG_DATA_HOME=$BANC/home/.local/share
export XDG_RUNTIME_DIR=$RUN
export DBUS_SESSION_BUS_ADDRESS=unix:path=$RUN/bus
export ODYSSEUS_DATA=$ROOT/data
export WLR_BACKENDS=headless
export WLR_RENDERER=pixman
export WLR_LIBINPUT_NO_DEVICES=1
export WAYLAND_DISPLAY=wayland-1
export SWAYSOCK=$RUN/sway.sock
export LC_ALL=C.UTF-8
export LIBGL_ALWAYS_SOFTWARE=1
export XKB_DEFAULT_LAYOUT=$DISPOSITION
export PATH=$BIN:\$PATH
EOF
# shellcheck disable=SC1091
. "$BANC/env.sh"
dbus-daemon --session --fork --address="unix:path=$RUN/bus" >/dev/null 2>&1
(sway -c "$BANC/sway.conf" > "$BANC/sway.log" 2>&1 &)
for _ in $(seq 1 30); do [ -S "$RUN/wayland-1" ] && break; sleep 0.2; done
[ -S "$RUN/wayland-1" ] || { echo "banc.sh : sway ne démarre pas, voir $BANC/sway.log" >&2; exit 1; }
banc-donnees
# Clavier et pointeur virtuels, branchés pour toute la session
(banc-entree --serveur --ecran "${W}x${H}" --disposition "$DISPOSITION" > "$BANC/entree.log" 2>&1 &)
for _ in $(seq 1 20); do [ -S "$RUN/banc-entree.sock" ] && break; sleep 0.1; done
[ -S "$RUN/banc-entree.sock" ] || { echo "banc.sh : banc-entree ne démarre pas, voir $BANC/entree.log" >&2; exit 1; }
sleep 0.3

# --- Outils des scènes -----------------------------------------------------------
arreter_tout() {
    for p in odysseus-assist odysseus-notes files odysseus-reglag compositor; do
        pkill -x "$p" || true
    done
}
cleanup() {
    if [ $garder -eq 1 ]; then
        say "Session gardée : . $BANC/env.sh  puis par exemple : odysseus-assistant &"
        say "Clavier et pointeur : banc-entree texte '…' | touche ctrl+h | clic X Y"
        say "Pour l'arrêter : banc-entree fin ; pkill -x sway"
    else
        arreter_tout
        pkill -x odysseus-notify || true
        banc-entree fin 2>/dev/null || pkill -x banc-entree || true
        pkill -x sway || true
        pkill -f "dbus-daemon --session --fork --address=unix:path=$RUN/bus" || true
    fi
}
trap cleanup EXIT

shot() { sleep "${2:-0.8}"; grim -o HEADLESS-1 "$CAPTURES/$1.png"; echo "  $1.png"; }
# Le pointeur en (x, y) ; « click [x y] » clique ; pointer_off le range dans un coin
pointer() { banc-entree pointeur "$1" "$2"; sleep 0.3; }
click() { banc-entree clic "$@"; }
pointer_off() { banc-entree pointeur $((W - 2)) $((H - 2)); }
# Clic dans la saisie de l'Assistant (lui donne le clavier), puis range le pointeur
focus_assistant() { click 1600 655; pointer_off; sleep 0.3; }
type_text() { banc-entree texte "$1"; }
keys() { banc-entree touche "$@"; }
erase() { keys $(printf 'BackSpace %.0s' $(seq 1 "$1")); }

# --- Scènes ----------------------------------------------------------------------
scene_assistant() {
    say "Scène : Assistant"
    printf 'Le disque Archives répond en 0,4 ms mais ne dépasse pas 2,1 Mo/s en lecture.' | wl-copy --primary
    printf 'rsync -av --delete ~/Projets/ /mnt/archives/projets/' | wl-copy
    sleep 0.3
    (odysseus-assistant > "$BANC/assistant.log" 2>&1 &); sleep 2.5
    shot assistant-01-ouverture
    focus_assistant
    type_text 'fire'; shot assistant-02-lancer
    erase 4; type_text 'firefox rame depuis ce matin'; shot assistant-03-question
    erase 30; sleep 0.3; keys ctrl+h; shot assistant-04-conversations
    keys Return; shot assistant-05-conversation 1.2
    pkill -x odysseus-assist; sleep 0.4
    (odysseus-assistant --notification "Firefox : Téléchargement terminé — rapport-trimestriel-2026.pdf (4,2 Mo)" > "$BANC/assistant2.log" 2>&1 &)
    shot assistant-06-depuis-notification 2.5
    pkill -x odysseus-assist; sleep 0.3
    wl-copy --clear; wl-copy --primary --clear
}

scene_reglages() {
    say "Scène : Réglages > Agent"
    (odysseus-reglages agent > "$BANC/reglages.log" 2>&1 &); sleep 3
    shot reglages-01-agent
    # La page entière, sur un écran plus haut
    swaymsg output HEADLESS-1 resolution "${W}x2300" >/dev/null; sleep 1.5
    shot reglages-02-agent-entier
    swaymsg output HEADLESS-1 resolution "${W}x${H}" >/dev/null; sleep 1
    pkill -x odysseus-reglag; sleep 0.3
}

scene_notifications() {
    say "Scène : notifications"
    pgrep -x odysseus-notify >/dev/null || { (odysseus-notify > "$BANC/notify.log" 2>&1 &); sleep 2; }
    odysseus-notify --send "Firefox" "Téléchargement terminé : rapport-trimestriel-2026.pdf (4,2 Mo)"
    shot notifications-01-banniere 0.8
    pointer 1700 55; shot notifications-02-banniere-survol
    pointer_off
    odysseus-notify --send "Réception" "L'iPhone de David envoie 3 photos (12 Mo)"; sleep 0.5
    odysseus-notify --center show; sleep 1.2
    pointer 1700 360; shot notifications-03-centre-survol 1.0
    pointer_off
    odysseus-notify --center >/dev/null 2>&1 || true; sleep 0.5
}

scene_fichiers() {
    say "Scène : Fichiers"
    (files "$HOME/Téléchargements" > "$BANC/files.log" 2>&1 &); sleep 3
    shot fichiers-01-telechargements
    pkill -x files; sleep 0.3
}

# Le compositeur odysseus lui-même, imbriqué (fenêtre winit, rendu logiciel),
# avec Notes ouvert dedans : une note tapée, l'Assistant appelé en contexte
# près de la fenêtre (Super+A), une conversation reprise, sa réponse insérée
# dans la note. Le rendu logiciel est lent : on laisse du temps aux captures.
scene_compositeur() {
    say "Scène : compositeur imbriqué"
    [ -x "$BIN/compositor" ] || { echo "  compositor absent : compilez avec --compositeur" >&2; return; }
    (compositor -c "odysseus-notes" > "$BANC/compositor.log" 2>&1 &); sleep 5
    local inner
    inner=$(ls "$RUN" | grep -E '^wayland-[0-9]+$' | grep -v '^wayland-1$' | head -1 || true)
    if [ -z "$inner" ]; then
        echo "  le compositeur n'a pas ouvert d'affichage, voir $BANC/compositor.log" >&2
        pkill -x compositor || true
        return
    fi
    click 700 600; sleep 0.5
    keys ctrl+n; sleep 0.5
    type_text 'Diagnostic du NAS Synology'; keys Return
    pointer_off
    shot compositeur-01-notes 2.5
    (WAYLAND_DISPLAY=$inner SWAYSOCK=$RUN/odysseus-ipc.$inner.sock odysseus-assistant --contexte > "$BANC/assistant3.log" 2>&1 &)
    shot compositeur-02-assistant-en-contexte 3.5
    click 1030 728; shot compositeur-03-conversation 2.0
    click 1073 606; pointer_off; shot compositeur-04-insere-dans-notes 3.0
    pkill -x odysseus-assist || true; sleep 0.3
    pkill -x compositor || true; sleep 0.5
}

for s in "${scenes[@]}"; do
    case $s in
        assistant|reglages|notifications|fichiers|compositeur) "scene_$s" ;;
        *) echo "banc.sh : scène inconnue $s" >&2 ;;
    esac
done
say "Captures dans $CAPTURES"
