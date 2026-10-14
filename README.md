# odysseus-desktop

Bureau Wayland personnel en Rust : rapide, sobre, inspiré de macOS sans le copier.

| Dossier | Rôle | État |
|---|---|---|
| `common/` | Thème (couleurs, grille de 4 px, typographie) et dessin logiciel partagé (`draw`) | base |
| `compositor/` | Compositeur Smithay : fenêtres arrondies + ombres, animations, bureaux 1–9, IPC compatible sway, backends winit et DRM | fonctionne (imbriqué et sur tty) |
| `shell-bar/` | Barre du haut : bureaux (clic, molette), heure, date | fonctionne sous sway et odysseus |
| `launcher/` | Lanceur façon Spotlight (Mod+Espace) | fonctionne |
| `files/` | Fichiers : explorateur (favoris, chemin cliquable, nom · taille · date) | fonctionne |

## Raccourcis (Mod = Super sur tty, Alt dans une fenêtre sway)

| Touches | Action |
|---|---|
| Mod+Entrée | terminal |
| Mod+Espace | lanceur |
| Mod+q | fermer la fenêtre |
| Mod+glisser | déplacer la fenêtre |
| Mod+1…9 / Mod+Maj+1…9 | aller au bureau / y envoyer la fenêtre |
| Impr. écran ou Mod+Maj+s | capture dans ~/Images |
| Ctrl+Alt+F1…F12 | changer de tty |
| Mod+Maj+e | quitter |

## Commandes

    ./odysseus-session.sh          # session complète sur un tty (sans sway)

    cargo build --release          # tout compiler
    cargo run -p shell-bar         # lancer la barre (depuis un terminal dans sway)
    cargo clippy --workspace       # vérifications

Pour lancer la barre au démarrage de sway, ajouter dans `~/.config/sway/config` :

    exec ~/odysseus-desktop/target/release/shell-bar
