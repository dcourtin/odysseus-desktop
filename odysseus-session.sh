#!/bin/sh
# Lance odysseus-desktop directement sur un tty (sans sway).
# Usage : depuis un tty connecté, `odysseus-session.sh` (dans le dépôt ou installé)
# Sortie : Super+Maj+e ; retour à un autre tty : Ctrl+Alt+F1…F12.

# Binaires : ODYSSEUS_BIN, sinon ceux du dépôt (target/release à côté de ce
# script), sinon le dossier de ce script (installation)
HERE=$(dirname "$(readlink -f "$0")")
if [ -n "${ODYSSEUS_BIN:-}" ]; then
    ROOT=$ODYSSEUS_BIN
elif [ -x "$HERE/target/release/compositor" ]; then
    ROOT=$HERE/target/release
else
    ROOT=$HERE
fi
# Journaux : ODYSSEUS_LOGDIR permet à une session de test de ne pas toucher aux vrais
LOGDIR="${ODYSSEUS_LOGDIR:-$HOME/.local/state/odysseus}"
mkdir -p "$LOGDIR"
# Journal de la session précédente gardé pour diagnostiquer un plantage
[ -f "$LOGDIR/compositor.log" ] && mv -f "$LOGDIR/compositor.log" "$LOGDIR/compositor.log.old"

export XKB_DEFAULT_LAYOUT="${XKB_DEFAULT_LAYOUT:-fr}"
# Curseur : Bibata s'il est installé, sinon Adwaita (présent presque partout)
if [ -z "${XCURSOR_THEME:-}" ]; then
    for theme in Bibata-Modern-Classic Adwaita default; do
        if [ -d "/usr/share/icons/$theme/cursors" ] || [ -d "$HOME/.local/share/icons/$theme/cursors" ]; then
            XCURSOR_THEME=$theme
            break
        fi
    done
fi
export XCURSOR_THEME
export XCURSOR_SIZE="${XCURSOR_SIZE:-24}"
# Écran et échelle : Réglages > Écran ; sans réglage, l'écran préféré et une
# échelle d'après sa densité (ODYSSEUS_OUTPUT, ODYSSEUS_SCALE pour forcer)
export XDG_SESSION_TYPE=wayland
export XDG_CURRENT_DESKTOP=odysseus
export DBUS_SESSION_BUS_ADDRESS="${DBUS_SESSION_BUS_ADDRESS:-unix:path=$XDG_RUNTIME_DIR/bus}"
# Firefox : Wayland natif
export MOZ_ENABLE_WAYLAND=1
export RUST_LOG="${RUST_LOG:-info}"
unset WAYLAND_DISPLAY DISPLAY

# Partage d'écran : xdg-desktop-portal-wlr demande à odysseus-partage quoi montrer
XDPW="${XDG_CONFIG_HOME:-$HOME/.config}/xdg-desktop-portal-wlr"
mkdir -p "$XDPW"
printf '# Écrit par odysseus-session.sh à chaque connexion\n[screencast]\nchooser_type=dmenu\nchooser_cmd=%s\nmax_fps=30\n' \
  "$ROOT/odysseus-partage" > "$XDPW/odysseus"

# Au démarrage : l'affichage est transmis au bus de session (le démon de notifications
# peut y être relancé à la demande, voir ~/.local/share/dbus-1/services), puis
# démons de notifications et des automatismes, Dock et barre
exec "$ROOT/compositor" -c "dbus-update-activation-environment WAYLAND_DISPLAY SWAYSOCK XDG_CURRENT_DESKTOP; '$ROOT/odysseus-notify' 2>>'$LOGDIR/notify.log' & '$ROOT/odysseus-auto' 2>>'$LOGDIR/auto.log' & '$ROOT/odysseus-dock' 2>>'$LOGDIR/dock.log' & exec '$ROOT/shell-bar'" > "$LOGDIR/compositor.log" 2>&1
