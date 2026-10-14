# odysseus-desktop

Bureau Wayland personnel en Rust : rapide, sobre, inspiré de macOS sans le copier.

| Dossier | Rôle | État |
|---|---|---|
| `common/` | Thème (couleurs, grille de 4 px, typographie) et dessin logiciel partagé (`draw`) | base |
| `compositor/` | Compositeur Smithay : fenêtres arrondies + ombres, animations, bureaux 1–9, IPC compatible sway, backends winit et DRM | fonctionne (imbriqué et sur tty) |
| `shell-bar/` | Barre du haut : bureaux (clic, molette) ; à droite icônes d'état, centre de contrôle, date et heure | fonctionne sous sway et odysseus |
| `notify/` | `odysseus-notify` : notifications (D-Bus standard), bannières, centre de notifications avec calendrier, « Ne pas déranger », indicateur de volume | fonctionne |
| `control/` | `odysseus-control` : centre de contrôle (Wi-Fi via iwd, Bluetooth via BlueZ, Ethernet, son via PipeWire, verrouiller, redémarrer, éteindre) | fonctionne |
| `lock/` | `odysseus-lock` : écran de verrouillage (ext-session-lock, PAM) | fonctionne |
| `launcher/` | Lanceur façon Spotlight (Mod+Espace) | fonctionne |
| `files/` | Fichiers : explorateur (favoris, chemin cliquable, nom · taille · date) | fonctionne |

## Raccourcis (Mod = Super sur tty, Alt dans une fenêtre sway)

| Touches | Action |
|---|---|
| Mod+Entrée | terminal |
| Mod+Espace | lanceur |
| Mod+q | fermer la fenêtre |
| Mod+glisser | déplacer la fenêtre |
| Mod+clic droit glissé | redimensionner |
| Mod+f | agrandir / restaurer |
| Mod+Tab | fenêtre suivante |
| Mod+1…9 / Mod+Maj+1…9 | aller au bureau / y envoyer la fenêtre |
| Impr. écran ou Mod+Maj+s | capture dans ~/Images |
| Mod+l | verrouiller |
| touches Volume +/−, Muet | volume (avec indicateur) |
| clic sur les icônes de la barre | centre de contrôle |
| clic sur l'heure | centre de notifications |
| Ctrl+Alt+F1…F12 | changer de tty |
| Mod+Maj+e | quitter |

Inactivité : l'écran s'éteint et la session se verrouille au bout de 10 min
(`ODYSSEUS_IDLE`, en secondes) ; une vidéo en lecture l'empêche.

## Commandes

    ./odysseus-session.sh          # session complète sur un tty (lancée d'office sur tty1, sway en secours sur tty2)

    odysseus-notify --send "Titre" "Texte"   # notification d'essai
    odysseus-notify --dnd toggle             # « Ne pas déranger »
    odysseus-control --vue wifi              # centre de contrôle, vue Wi-Fi

    cargo build --release          # tout compiler
    cargo run -p shell-bar         # lancer la barre (depuis un terminal dans sway)
    cargo clippy --workspace       # vérifications

Pour lancer la barre au démarrage de sway, ajouter dans `~/.config/sway/config` :

    exec ~/odysseus-desktop/target/release/shell-bar
