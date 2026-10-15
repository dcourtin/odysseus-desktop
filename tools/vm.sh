#!/bin/sh
# Machine virtuelle d'essai (qemu + KVM), pour porter odysseus sur une
# distribution courante. Une Debian stable installée sans intervention
# (preseed) sert de base propre ; chaque essai part d'une copie jetable.
#
#   tools/vm.sh installer     installe la base (sans écran, ~15 min, réseau)
#   tools/vm.sh neuf          repart d'une Debian neuve (copie jetable effacée)
#   tools/vm.sh lancer        démarre l'essai dans une fenêtre
#   tools/vm.sh sans-ecran    démarre l'essai sans fenêtre (piloté par ssh)
#   tools/vm.sh ssh [cmd]     se connecte à l'essai (utilisateur odysseus)
#   tools/vm.sh arreter       éteint l'essai proprement
#   tools/vm.sh etat          dit si la VM tourne
#
# Utilisateur de la VM : odysseus / odysseus (sudo sans mot de passe) ; ssh
# seulement depuis cette machine (127.0.0.1:2222), avec une clé dédiée.
set -eu

DIR=${ODYSSEUS_VM:-$HOME/VMs/debian}
MIRROR=https://deb.debian.org/debian
NETBOOT=$MIRROR/dists/stable/main/installer-amd64/current/images
PORT=2222
MEM=4096
CPUS=4

mkdir -p "$DIR"
cd "$DIR"

# Paramètres communs : KVM, processeur de l'hôte, disque et réseau virtio
qemu_base() {
    echo "-enable-kvm -cpu host -m $MEM -smp $CPUS -machine q35 \
-netdev user,id=net0,hostfwd=tcp:127.0.0.1:$PORT-:22 -device virtio-net-pci,netdev=net0 \
-device virtio-rng-pci -pidfile $DIR/qemu.pid"
}

running() {
    [ -f qemu.pid ] && kill -0 "$(cat qemu.pid)" 2>/dev/null
}

fetch() {
    # Noyau et initrd de l'installateur, vérifiés par leur somme SHA-256
    curl -fsSL "$NETBOOT/SHA256SUMS" -o SHA256SUMS
    for f in linux initrd.gz; do
        curl -fsSL "$NETBOOT/netboot/debian-installer/amd64/$f" -o "$f"
        want=$(grep " ./netboot/debian-installer/amd64/$f\$" SHA256SUMS | cut -d' ' -f1)
        got=$(sha256sum "$f" | cut -d' ' -f1)
        [ "$want" = "$got" ] || { echo "vm.sh : somme de contrôle fausse pour $f" >&2; exit 1; }
    done
}

preseed() {
    [ -f cle ] || ssh-keygen -q -t ed25519 -N '' -C odysseus-vm -f cle
    key=$(cat cle.pub)
    cat > preseed.cfg <<EOF
d-i debian-installer/language string en
d-i debian-installer/country string FR
d-i debian-installer/locale string en_US.UTF-8
d-i localechooser/supported-locales multiselect fr_FR.UTF-8
d-i keyboard-configuration/xkb-keymap select fr
d-i netcfg/choose_interface select auto
d-i netcfg/get_hostname string odysseus-vm
d-i netcfg/get_domain string
d-i mirror/country string manual
d-i mirror/http/hostname string deb.debian.org
d-i mirror/http/directory string /debian
d-i mirror/http/proxy string
d-i passwd/root-login boolean false
d-i passwd/user-fullname string Essai odysseus
d-i passwd/username string odysseus
d-i passwd/user-password password odysseus
d-i passwd/user-password-again password odysseus
d-i user-setup/allow-password-weak boolean true
d-i clock-setup/utc boolean true
d-i time/zone string Europe/Paris
d-i clock-setup/ntp boolean true
d-i partman-auto/disk string /dev/vda
d-i partman-auto/method string regular
d-i partman-auto/choose_recipe select atomic
d-i partman-partitioning/confirm_write_new_label boolean true
d-i partman/choose_partition select finish
d-i partman/confirm boolean true
d-i partman/confirm_nooverwrite boolean true
d-i apt-setup/non-free-firmware boolean true
tasksel tasksel/first multiselect standard, ssh-server
d-i pkgsel/upgrade select full-upgrade
popularity-contest popularity-contest/participate boolean false
d-i grub-installer/only_debian boolean true
d-i grub-installer/bootdev string /dev/vda
d-i preseed/late_command string mkdir -p /target/home/odysseus/.ssh; echo '$key' > /target/home/odysseus/.ssh/authorized_keys; in-target chown -R odysseus:odysseus /home/odysseus/.ssh; in-target chmod 700 /home/odysseus/.ssh; echo 'odysseus ALL=(ALL) NOPASSWD:ALL' > /target/etc/sudoers.d/odysseus; chmod 440 /target/etc/sudoers.d/odysseus; in-target update-locale LANG=fr_FR.UTF-8
d-i finish-install/reboot_in_progress note
d-i debian-installer/exit/poweroff boolean true
EOF
    # Le fichier de réponses rejoint l'initrd (archive cpio « newc » ajoutée à la suite)
    python3 - preseed.cfg <<'PY'
import gzip, sys
data = open(sys.argv[1], "rb").read()
def entry(name, body, mode):
    name = name.encode() + b"\0"
    head = "070701" + "".join("%08x" % v for v in (
        0, mode, 0, 0, 1, 0, len(body), 0, 0, 0, 0, len(name), 0))
    out = head.encode() + name
    out += b"\0" * (-len(out) % 4) + body
    return out + b"\0" * (-len(out) % 4)
cpio = entry("preseed.cfg", data, 0o100644) + entry("TRAILER!!!", b"", 0)
open("preseed.cpio.gz", "wb").write(gzip.compress(cpio))
PY
    cat initrd.gz preseed.cpio.gz > initrd-auto.gz
}

case "${1:-}" in
installer)
    running && { echo "vm.sh : une VM tourne déjà" >&2; exit 1; }
    fetch
    preseed
    rm -f base.qcow2 essai.qcow2
    qemu-img create -q -f qcow2 base.qcow2 20G
    # La console série n'affiche que l'anglais : installation en anglais, système
    # en français (locale fr_FR, clavier fr). qemu se détache : l'installation
    # continue sans ce terminal ; la VM s'éteint d'elle-même une fois finie.
    echo "Installation de Debian (sans écran) : suivi dans $DIR/installation.log"
    # shellcheck disable=SC2046
    qemu-system-x86_64 $(qemu_base) -drive file=base.qcow2,if=virtio \
        -kernel linux -initrd initrd-auto.gz \
        -append "auto=true priority=critical language=en country=FR locale=en_US.UTF-8 keymap=fr hostname=odysseus-vm domain= console=ttyS0,115200n8" \
        -display none -serial file:installation.log -no-reboot -daemonize
    echo "Elle s'éteint d'elle-même une fois finie (vm.sh etat) ; la base : $DIR/base.qcow2"
    ;;
neuf)
    running && { echo "vm.sh : éteignez d'abord l'essai (vm.sh arreter)" >&2; exit 1; }
    [ -f base.qcow2 ] || { echo "vm.sh : pas de base, lancez d'abord : vm.sh installer" >&2; exit 1; }
    rm -f essai.qcow2
    qemu-img create -q -f qcow2 -b base.qcow2 -F qcow2 essai.qcow2
    echo "Essai neuf : $DIR/essai.qcow2"
    ;;
lancer | sans-ecran)
    running && { echo "vm.sh : l'essai tourne déjà" >&2; exit 1; }
    [ -f essai.qcow2 ] || "$0" neuf
    if [ "$1" = lancer ]; then
        # Fenêtre et son par PipeWire, dans la session graphique. Sans
        # accélération : virgl demande l'OpenGL de bureau (libGL), que la Mesa
        # Wayland seule de cette machine n'a pas ; le rendu se fait par le processeur.
        display="-device virtio-vga -display gtk \
-audiodev pipewire,id=son -device intel-hda -device hda-duplex,audiodev=son"
    else
        display="-display none -serial file:$DIR/console.log -daemonize"
    fi
    # shellcheck disable=SC2046,SC2086
    qemu-system-x86_64 $(qemu_base) -drive file=essai.qcow2,if=virtio $display
    ;;
ssh)
    shift
    exec ssh -i "$DIR/cle" -p $PORT -o StrictHostKeyChecking=no \
        -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR odysseus@127.0.0.1 "$@"
    ;;
etat)
    if running; then
        echo "La VM tourne (pid $(cat qemu.pid))"
    else
        echo "La VM est éteinte"
    fi
    ;;
arreter)
    "$0" ssh sudo poweroff || true
    ;;
*)
    sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac
