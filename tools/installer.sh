#!/bin/sh
# Installe odysseus depuis ce dépôt, sur n'importe quelle distribution.
#
#   tools/installer.sh [--prefix DIR] [--tty1] [--sans-compiler]
#   tools/installer.sh --desinstaller [--prefix DIR]
#
# À lancer en utilisateur ordinaire : la compilation se fait à son nom, les
# copies système passent par sudo. Par défaut, préfixe /usr/local :
#   PREFIX/lib/odysseus        binaires (noms courts : hors du PATH)
#   PREFIX/bin/odysseus-session  lancement de la session
#   PREFIX/share/odysseus      polices, icônes
#   PREFIX/share/applications  entrées du lanceur
#   PREFIX/share/dbus-1        notifications, apparence (activés à la demande)
# et, hors préfixe (emplacements lus par les services) : portails
# (/etc/xdg-desktop-portal, /usr/share/xdg-desktop-portal/portals), PAM du
# verrouillage (/etc/pam.d/odysseus-lock), session pour les gestionnaires de
# connexion (/usr/share/wayland-sessions).
#
# --tty1 : la session démarre à la connexion sur tty1 (~/.bash_profile).
# Ce qui est posé est noté dans PREFIX/share/odysseus/installe.txt.
set -eu

REPO=$(cd "$(dirname "$0")/.." && pwd)
PREFIX=/usr/local
TTY1=non
BUILD=oui
MODE=installer
while [ $# -gt 0 ]; do
    case "$1" in
    --prefix) PREFIX=$2; shift ;;
    --tty1) TTY1=oui ;;
    --sans-compiler) BUILD=non ;;
    --desinstaller) MODE=desinstaller ;;
    *) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
    esac
    shift
done

LIB=$PREFIX/lib/odysseus
SHARE=$PREFIX/share/odysseus
MANIFEST=$SHARE/installe.txt
if [ "$(id -u)" = 0 ]; then SUDO=; else SUDO=sudo; fi

say() { printf '%s\n' "$*"; }

# --- Désinstaller -------------------------------------------------------------
if [ "$MODE" = desinstaller ]; then
    [ -f "$MANIFEST" ] || { say "Rien à désinstaller dans $PREFIX"; exit 1; }
    # Fichiers d'abord, puis les dossiers devenus vides
    sort -r "$MANIFEST" | while read -r f; do
        [ -d "$f" ] && [ ! -L "$f" ] || $SUDO rm -f "$f"
    done
    $SUDO rm -rf "$LIB" "$SHARE"
    say "odysseus désinstallé de $PREFIX"
    exit 0
fi

# --- Dépendances ----------------------------------------------------------------
missing=""
for pair in foot:foot wl-copy:wl-clipboard rsvg-convert:librsvg2-bin \
    pipewire:pipewire wireplumber:wireplumber wpctl:wireplumber \
    dbus-update-activation-environment:dbus-bin \
    /usr/libexec/xdg-desktop-portal:xdg-desktop-portal \
    /usr/libexec/xdg-desktop-portal-wlr:xdg-desktop-portal-wlr; do
    cmd=${pair%%:*}
    pkg=${pair#*:}
    case "$cmd" in
    /*) [ -x "$cmd" ] || missing="$missing $pkg" ;;
    *) command -v "$cmd" >/dev/null 2>&1 || missing="$missing $pkg" ;;
    esac
done
if [ -n "$missing" ]; then
    say "À installer pour une session complète (noms Debian) :$missing"
fi

# --- Compiler -------------------------------------------------------------------
cd "$REPO"
if [ "$BUILD" = oui ]; then
    say "Compilation (cargo build --release)…"
    cargo build --release --quiet
fi
# Binaires du dépôt : ceux que cargo vient de produire
bins=$(cargo build --release --quiet --message-format=json 2>/dev/null |
    grep -o '"executable":"[^"]*"' | sed 's/.*:"\(.*\)"/\1/' | sort -u)
[ -n "$bins" ] || { say "installer.sh : aucun binaire trouvé" >&2; exit 1; }

# --- Copier ---------------------------------------------------------------------
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
put() { # put SOURCE DEST [MODE] : copie, et note dans le manifeste
    $SUDO install -D -m "${3:-644}" "$1" "$2"
    echo "$2" >> "$tmp"
}
gen() { # gen DEST [MODE] < contenu
    cat > "$tmp.contenu"
    put "$tmp.contenu" "$1" "${2:-644}"
    rm -f "$tmp.contenu"
}

for b in $bins; do
    put "$b" "$LIB/$(basename "$b")" 755
done
put odysseus-session.sh "$LIB/odysseus-session.sh" 755
$SUDO mkdir -p "$PREFIX/bin"
$SUDO ln -sf "$LIB/odysseus-session.sh" "$PREFIX/bin/odysseus-session"
echo "$PREFIX/bin/odysseus-session" >> "$tmp"

for f in data/fonts/*; do
    put "$f" "$SHARE/fonts/$(basename "$f")"
done
for f in */data/*.svg; do
    put "$f" "$SHARE/icones/$(basename "$f")"
done
for f in */data/*.desktop; do
    sed -e "s#@BIN@#$LIB#g" -e "s#@ICONES@#$SHARE/icones#g" "$f" |
        gen "$PREFIX/share/applications/$(basename "$f")"
done

# Démon de notifications : aussi le backend d'apparence des portails
gen "$PREFIX/share/dbus-1/services/org.freedesktop.Notifications.service" <<EOF
[D-BUS Service]
Name=org.freedesktop.Notifications
Exec=$LIB/odysseus-notify
EOF
gen "$PREFIX/share/dbus-1/services/org.freedesktop.impl.portal.desktop.odysseus.service" <<EOF
[D-BUS Service]
Name=org.freedesktop.impl.portal.desktop.odysseus
Exec=$LIB/odysseus-notify
EOF
gen /etc/xdg-desktop-portal/odysseus-portals.conf <<EOF
[preferred]
default=none
org.freedesktop.impl.portal.Settings=odysseus
org.freedesktop.impl.portal.ScreenCast=wlr
org.freedesktop.impl.portal.Screenshot=wlr
EOF
gen /usr/share/xdg-desktop-portal/portals/odysseus.portal <<EOF
[portal]
DBusName=org.freedesktop.impl.portal.desktop.odysseus
Interfaces=org.freedesktop.impl.portal.Settings;
UseIn=odysseus
EOF

# Verrouillage : le mot de passe de l'utilisateur, selon la distribution ;
# un fichier déjà posé par odysseus est gardé (et noté pour la désinstallation)
if grep -q "verrouillage d'odysseus" /etc/pam.d/odysseus-lock 2>/dev/null; then
    echo /etc/pam.d/odysseus-lock >> "$tmp"
elif [ ! -e /etc/pam.d/odysseus-lock ]; then
    if [ -e /etc/pam.d/common-auth ]; then
        printf '%s\n' "# Écran de verrouillage d'odysseus : mot de passe de l'utilisateur" \
            "@include common-auth" "@include common-account" | gen /etc/pam.d/odysseus-lock
    else
        printf '%s\n' "# Écran de verrouillage d'odysseus : mot de passe de l'utilisateur" \
            "auth      include     system-auth" "account   include     system-account" |
            gen /etc/pam.d/odysseus-lock
    fi
fi

gen /usr/share/wayland-sessions/odysseus.desktop <<EOF
[Desktop Entry]
Name=odysseus
Comment=Poste de pilotage minimaliste
Exec=$PREFIX/bin/odysseus-session
Type=Application
DesktopNames=odysseus
EOF

# Manifeste (avant lui-même)
echo "$MANIFEST" >> "$tmp"
$SUDO install -D -m 644 "$tmp" "$MANIFEST"

# --- Session sur tty1 -----------------------------------------------------------
if [ "$TTY1" = oui ]; then
    line="if [ \"\$(tty)\" = /dev/tty1 ]; then exec $PREFIX/bin/odysseus-session; fi"
    profile=$HOME/.bash_profile
    if [ ! -e "$profile" ] && [ -e "$HOME/.profile" ]; then
        echo '[ -f ~/.profile ] && . ~/.profile' > "$profile"
    fi
    # Une ancienne ligne odysseus est remplacée
    if [ -e "$profile" ]; then
        grep -v 'odysseus-session' "$profile" > "$tmp.profil" || true
        cat "$tmp.profil" > "$profile"
        rm -f "$tmp.profil"
    fi
    echo "$line" >> "$profile"
    say "Session odysseus au démarrage de tty1 (~/.bash_profile)"
fi

say "odysseus installé dans $PREFIX ($(wc -l < "$MANIFEST") fichiers) ; session : $PREFIX/bin/odysseus-session"
