#!/bin/sh
# Lance odysseus-desktop directement sur un tty (sans sway).
# Usage : depuis un tty connecté, `~/odysseus-desktop/odysseus-session.sh`
# Sortie : Super+Maj+e ; retour à un autre tty : Ctrl+Alt+F1…F12.

ROOT="$HOME/odysseus-desktop/target/release"
LOGDIR="$HOME/.local/state/odysseus"
mkdir -p "$LOGDIR"

export XKB_DEFAULT_LAYOUT="${XKB_DEFAULT_LAYOUT:-fr}"
export XCURSOR_THEME="${XCURSOR_THEME:-Bibata-Modern-Classic}"
export XCURSOR_SIZE="${XCURSOR_SIZE:-24}"
export ODYSSEUS_OUTPUT="${ODYSSEUS_OUTPUT:-HDMI-A-1}"
export ODYSSEUS_SCALE="${ODYSSEUS_SCALE:-1.5}"
export XDG_SESSION_TYPE=wayland
export RUST_LOG="${RUST_LOG:-info}"
unset WAYLAND_DISPLAY DISPLAY

exec "$ROOT/compositor" -c "$ROOT/shell-bar & foot" > "$LOGDIR/compositor.log" 2>&1
