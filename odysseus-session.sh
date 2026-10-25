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
# (une telle session ne touche pas non plus à la cible systemd de la vraie, voir plus bas)
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

# Cible systemd de la session : les portails (xdg-desktop-portal, qui donne clair ou
# sombre et l'accent à Firefox et Obsidian, et le partage d'écran) exigent que
# graphical-session.target soit active (Requisite). Cette cible ne se démarre pas à la
# main : odysseus-session.target la tire, démarrée une fois le compositeur prêt et
# arrêtée à la sortie. Pas pour une session d'essai : la cible est celle de la vraie.
UNITS="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
TARGET=""
if [ -z "${ODYSSEUS_LOGDIR:-}" ] && command -v systemctl >/dev/null 2>&1; then
    TARGET=odysseus-session.target
    unit="[Unit]
Description=Session odysseus
Documentation=https://github.com/dcourtin/odysseus-desktop
BindsTo=graphical-session.target
Wants=graphical-session-pre.target
After=graphical-session-pre.target
"
    if [ "$(cat "$UNITS/$TARGET" 2>/dev/null)" != "$(printf '%s' "$unit")" ]; then
        mkdir -p "$UNITS"
        printf '%s' "$unit" > "$UNITS/$TARGET"
        systemctl --user daemon-reload 2>/dev/null
    fi
fi
START_TARGET=""
[ -n "$TARGET" ] && START_TARGET="systemctl --user start $TARGET 2>>'$LOGDIR/session.log';"

# Premier démarrage : l'accueil (Réglages en mode pas à pas), tant qu'il n'a pas été terminé
WELCOME=""
if [ ! -e "${XDG_CONFIG_HOME:-$HOME/.config}/odysseus/accueil" ]; then
    WELCOME="(sleep 2; '$ROOT/odysseus-reglages' --accueil) 2>>'$LOGDIR/accueil.log' &"
fi

# Au démarrage : l'affichage est transmis au bus de session et à systemd (le démon de
# notifications et les portails y sont lancés à la demande, voir
# ~/.local/share/dbus-1/services), la cible de session est démarrée, puis démons de
# notifications, des automatismes et de la réception de fichiers, Dock et barre
"$ROOT/compositor" -c "dbus-update-activation-environment --systemd WAYLAND_DISPLAY SWAYSOCK XDG_CURRENT_DESKTOP XDG_SESSION_TYPE; $START_TARGET '$ROOT/odysseus-notify' 2>>'$LOGDIR/notify.log' & '$ROOT/odysseus-auto' 2>>'$LOGDIR/auto.log' & '$ROOT/odysseus-reception' 2>>'$LOGDIR/reception.log' & '$ROOT/odysseus-dock' 2>>'$LOGDIR/dock.log' & $WELCOME exec '$ROOT/shell-bar'" > "$LOGDIR/compositor.log" 2>&1
status=$?
# Sortie de session : les portails s'arrêtent avec la cible
[ -n "$TARGET" ] && systemctl --user stop "$TARGET" 2>>"$LOGDIR/session.log"
exit $status
