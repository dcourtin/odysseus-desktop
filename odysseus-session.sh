#!/bin/sh
# Lance odysseus-desktop directement sur un tty (sans sway).
# Usage : depuis un tty connecté, `~/odysseus-desktop/odysseus-session.sh`
# Sortie : Super+Maj+e ; retour à un autre tty : Ctrl+Alt+F1…F12.

ROOT="$HOME/odysseus-desktop/target/release"
# Journaux : ODYSSEUS_LOGDIR permet à une session de test de ne pas toucher aux vrais
LOGDIR="${ODYSSEUS_LOGDIR:-$HOME/.local/state/odysseus}"
mkdir -p "$LOGDIR"
# Journal de la session précédente gardé pour diagnostiquer un plantage
[ -f "$LOGDIR/compositor.log" ] && mv -f "$LOGDIR/compositor.log" "$LOGDIR/compositor.log.old"

export XKB_DEFAULT_LAYOUT="${XKB_DEFAULT_LAYOUT:-fr}"
export XCURSOR_THEME="${XCURSOR_THEME:-Bibata-Modern-Classic}"
export XCURSOR_SIZE="${XCURSOR_SIZE:-24}"
export ODYSSEUS_OUTPUT="${ODYSSEUS_OUTPUT:-HDMI-A-1}"
export ODYSSEUS_SCALE="${ODYSSEUS_SCALE:-1.5}"
export XDG_SESSION_TYPE=wayland
export XDG_CURRENT_DESKTOP=odysseus
export DBUS_SESSION_BUS_ADDRESS="${DBUS_SESSION_BUS_ADDRESS:-unix:path=$XDG_RUNTIME_DIR/bus}"
# Firefox : Wayland natif
export MOZ_ENABLE_WAYLAND=1
export RUST_LOG="${RUST_LOG:-info}"
unset WAYLAND_DISPLAY DISPLAY

# Au démarrage : l'affichage est transmis au bus de session (le démon de notifications
# peut y être relancé à la demande, voir ~/.local/share/dbus-1/services), puis
# démons de notifications et des automatismes, et barre
exec "$ROOT/compositor" -c "dbus-update-activation-environment WAYLAND_DISPLAY SWAYSOCK XDG_CURRENT_DESKTOP; '$ROOT/odysseus-notify' 2>>'$LOGDIR/notify.log' & '$ROOT/odysseus-auto' 2>>'$LOGDIR/auto.log' & exec '$ROOT/shell-bar'" > "$LOGDIR/compositor.log" 2>&1
