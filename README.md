# odysseus-desktop

Bureau Wayland personnel en Rust : rapide, sobre, inspiré de macOS sans le copier.

> ⚠️ **Travail d'exploration — à ne pas utiliser en production.**
> Ce dépôt est un terrain d'expérimentation personnel, construit pour une seule
> machine (Linux From Scratch, GPU AMD, écran 4K, clavier AZERTY). Il n'est ni
> audité, ni testé au-delà de cette machine, ni maintenu pour d'autres usages :
> l'écran de verrouillage, la gestion de session et le compositeur n'offrent
> aucune garantie de sécurité ou de stabilité. Les chemins, réglages et choix
> matériels y sont souvent codés en dur. À lire, à piocher, à s'en inspirer —
> pas à déployer.
>
> *Exploratory work — not for production use. Personal experiment built for a
> single machine; no security or stability guarantees.*

## Aperçu

![Bureau : Fichiers, barre et notifications](docs/captures/bureau.png)

| Centre de contrôle | Centre de notifications |
|---|---|
| ![Centre de contrôle](docs/captures/centre-controle.png) | ![Centre de notifications](docs/captures/centre-notifications.png) |

| Lanceur (Super+Espace) | Fichiers |
|---|---|
| ![Lanceur](docs/captures/lanceur.png) | ![Fichiers](docs/captures/fichiers.png) |

| Aperçu (visionneuse d'images) |
|---|
| ![Aperçu](docs/captures/apercu.png) |

![Écran de verrouillage](docs/captures/verrou.png)

*Captures faites par le compositeur lui-même (Impr. écran), en 4K à l'échelle 1,5,
avec le fond d'écran dégradé par défaut. Notifications et disque « Archives » de démonstration ;
l'image ouverte dans Aperçu est une fractale de Mandelbrot générée par FFmpeg.*

| Dossier | Rôle | État |
|---|---|---|
| `common/` | Thème (couleurs, grille de 4 px, typographie) et dessin logiciel partagé (`draw`) | base |
| `compositor/` | Compositeur Smithay : fenêtres arrondies + ombres, animations (ouverture, fermeture, glissement entre bureaux, trajets d'aimantation), aimantation des fenêtres, bureaux 1–9, IPC compatible sway, backends winit et DRM | fonctionne (imbriqué et sur tty) |
| `shell-bar/` | Barre du haut : bureaux (clic, molette) ; à droite le centre de contrôle, la date et l'heure | fonctionne sous sway et odysseus |
| `notify/` | `odysseus-notify` : notifications (D-Bus standard), bannières, centre de notifications avec calendrier, « Ne pas déranger », indicateur de volume | fonctionne |
| `control/` | `odysseus-control` : centre de contrôle (Wi-Fi via iwd, Bluetooth via BlueZ, Ethernet, lumière de nuit, son via PipeWire, veille, verrouiller, redémarrer, éteindre) | fonctionne |
| `lock/` | `odysseus-lock` : écran de verrouillage (ext-session-lock, PAM) | fonctionne |
| `launcher/` | Lanceur façon Spotlight (Mod+Espace) | fonctionne |
| `files/` | Fichiers : explorateur (favoris, corbeille, appareils USB avec éjection, chemin cliquable, nom · taille · date) | fonctionne |
| `preview/` | `apercu` : visionneuse d'images (dossier parcouru aux flèches, zoom vers le pointeur, rotation, recadrage, infos EXIF, corbeille) | fonctionne |

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
| glisser une fenêtre contre un bord | aimantation : moitié (gauche/droite), plein (haut), quart (coins) ; la décrocher lui rend sa taille |
| Mod+← / Mod+→ / Mod+↑ / Mod+↓ | moitié gauche / droite / agrandir / restaurer |
| Mod+1…9 / Mod+Maj+1…9 | aller au bureau / y envoyer la fenêtre |
| Impr. écran ou Mod+Maj+s | capture d'une zone (glisser) ; Espace : d'une fenêtre (avec son ombre) ; Échap : annuler |
| Maj+Impr. écran | capture de l'écran entier |
| Mod+l | verrouiller |
| touches Volume +/−, Muet | volume (avec indicateur) |
| clic sur les icônes de la barre | centre de contrôle |
| clic sur l'heure | centre de notifications |
| Ctrl+Alt+F1…F12 | changer de tty |
| Mod+Maj+e | quitter |

Écran : branché à chaud (ODYSSEUS_OUTPUT est l'écran préféré, sinon le premier
connecté). Par IPC (`SWAYSOCK`) : `output scale 1.25`, `output mode 2560x1440@60`,
`nightlight on|off|auto|toggle` (auto : 21 h → 7 h, `ODYSSEUS_NIGHT_FROM/TO/TEMP`).

Fichiers et Aperçu : Espace (aperçu rapide d'une image), Suppr (corbeille, au
format freedesktop), Ctrl+Z (annuler). Aperçu : ← → (image précédente / suivante),
molette (zoom), 0 / 1 (ajuster / taille réelle), R (pivoter), C puis Entrée
(recadrer), I (infos), F (plein écran), Ctrl+S / Ctrl+Maj+S (enregistrer / une copie).

Captures d'écran : enregistrées dans `~/Images` (« Capture d’écran AAAA-MM-JJ à HH.MM.SS.png »),
copiées dans le presse-papiers et annoncées par une notification avec vignette (clic : Aperçu).

Animations : `ODYSSEUS_ANIM_SLOW=10` les ralentit toutes dix fois (pour les régler à l'œil).

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

## Licence

MIT — © 2026 David Courtin. Voir [LICENSE](LICENSE). Le compositeur est parti de
*smallvil* (Smithay, MIT) : voir `compositor/LICENSE-smithay.txt`.
