# odysseus-desktop

Bureau Wayland personnel en Rust : rapide, sobre.

> ⚠️ **Travail d'exploration — à ne pas utiliser en production.**
> Ce dépôt est un terrain d'expérimentation personnel, construit pour une seule
> machine (Linux From Scratch, GPU AMD, écran 4K, clavier AZERTY). Il n'est ni
> audité, ni testé au-delà de cette machine, ni maintenu pour d'autres usages :
> l'écran de verrouillage, la gestion de session et le compositeur n'offrent
> aucune garantie de sécurité ou de stabilité. Les chemins, réglages et choix
> matériels y sont souvent codés en dur.
>
> *Exploratory work — not for production use. Personal experiment built for a
> single machine; no security or stability guarantees.*

## Aperçu

![Bureau : Fichiers avec son panneau d'informations, notifications](docs/captures/bureau.png)

| Centre de contrôle | Centre de notifications |
|---|---|
| ![Centre de contrôle](docs/captures/centre-controle.png) | ![Centre de notifications](docs/captures/centre-notifications.png) |

| Moniteur système |
|---|
| ![Moniteur système](docs/captures/moniteur.png) |

| Fichiers (tri, panneau d'informations) | Lanceur (Super+Espace) |
|---|---|
| ![Fichiers](docs/captures/fichiers.png) | ![Lanceur](docs/captures/lanceur.png) |

| Réglages : fond d'écran, et six autres pages |
|---|
| ![Réglages](docs/captures/reglages.png) |

| Aide-mémoire des raccourcis (Super+K) |
|---|
| ![Raccourcis](docs/captures/raccourcis.png) |

| Thèmes : Odyssée bleue, sombre et claire |
|---|
| ![Thèmes](docs/captures/themes.png) |

| Aperçu (visionneuse d'images) |
|---|
| ![Aperçu](docs/captures/apercu.png) |

![Écran de verrouillage](docs/captures/verrou.png)

*Captures faites par le compositeur lui-même, en 4K à l'échelle 1,5, avec un fond
dégradé de démonstration. Notifications, dossier « Démo » et disque « Archives » de
démonstration ; les images sont des fractales de Mandelbrot générées par FFmpeg. Dans
la barre, l'icône du panneau ouvert est en surbrillance.*

| Dossier | Rôle | État |
|---|---|---|
| `common/` | Thème (couleurs, grille de 4 px, typographie) et dessin logiciel partagé (`draw`) | base |
| `compositor/` | Compositeur Smithay : fenêtres arrondies + ombres, animations (ouverture, fermeture, glissement entre bureaux, trajets d'aimantation), aimantation des fenêtres, bureaux 1–9, coin actif (haut droit : notifications), glisser-déposer entre applications, presse-papiers accessible en ligne de commande (data-control), captures d'écran, lumière de nuit, IPC compatible sway, backends winit et DRM | fonctionne (imbriqué et sur tty) |
| `shell-bar/` | Barre du haut : bureaux (clic, molette) ; à droite moniteur (jauge colorée selon la charge), centre de contrôle, cloche des notifications (point : non vues), date et heure ; l'icône du panneau ouvert est en surbrillance | fonctionne sous sway et odysseus |
| `notify/` | `odysseus-notify` : notifications (D-Bus standard), bannières, centre de notifications avec calendrier (cloche, heure ou coin actif), « Ne pas déranger », indicateur de volume ; portail des réglages d'apparence (Firefox et GTK suivent le thème clair ou sombre) | fonctionne |
| `control/` | `odysseus-control` : centre de contrôle ; `odysseus-raccourcis` : aide-mémoire des raccourcis ; (Wi-Fi via iwd, Bluetooth via BlueZ, Ethernet, lumière de nuit, son via PipeWire, accès aux Réglages, veille, verrouiller, redémarrer, éteindre) | fonctionne |
| `settings/` | `odysseus-reglages` : Réglages — Apparence (thèmes), Fond d'écran (dégradés d'odysseus calculés en 4K, images de l'utilisateur, dépôt depuis Fichiers), Écran (définition avec confirmation et retour automatique, fréquence, taille, lumière de nuit : horaires et teinte), Clavier et langue (disposition, répétition, zone d'essai, langue du système via systemd-localed), Énergie (extinction, verrouillage, veille), Raccourcis, Agent (Claude Code et Ollama installés, mis à jour et connectés depuis la page, modèles téléchargés avec progression, autorisations d'odysseus-mcp, activité ; tout vient de `~/.config/odysseus/agent.json`, modifiable à la main), À propos | fonctionne |
| `mcp/` | `odysseus-mcp` : le bureau exposé aux agents (Model Context Protocol) — état du système, écran, réglages, journaux, fenêtres, notifications, captures, actions (notifier, ouvrir, bureaux, thème, fond, lumière de nuit, échelle), presse-papiers ; chaque catégorie autorisée ou non dans Réglages > Agent, chaque appel consigné | fonctionne |
| `lock/` | `odysseus-lock` : écran de verrouillage (ext-session-lock, PAM) | fonctionne |
| `launcher/` | Lanceur façon Spotlight (Mod+Espace) | fonctionne |
| `sysmon/` | `odysseus-moniteur` : moniteur système (icône jauge de la barre, colorée selon la charge) — processeur (charge, cœurs, fréquence, température), mémoire, disques (place, débits), carte graphique AMD (charge, mémoire vidéo, température, consommation), réseau, processus les plus actifs (arrêt en deux clics) ; courbes sur 60 s | fonctionne |
| `files/` | Fichiers : explorateur (favoris, corbeille, appareils USB avec éjection, chemin cliquable, tri par nom · taille · date, panneau d'informations avec vignette et actions selon le type — décompresser une archive, terminal dans un dossier —, glisser-déposer vers d'autres fenêtres ou dans un dossier) | fonctionne |
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
| Mod+k (ou Réglages > Raccourcis) | aide-mémoire des raccourcis |
| touches Volume +/−, Muet | volume (avec indicateur) |
| icônes de la barre (de gauche à droite) | moniteur système, centre de contrôle (dont l'accès aux Réglages) |
| cloche de la barre, clic sur l'heure, ou souris dans le coin en haut à droite | centre de notifications (point sur la cloche : notifications non vues) |
| Ctrl+Alt+F1…F12 | changer de tty |
| Mod+Maj+e | quitter |

Réglages (`odysseus-reglages`, ou centre de contrôle > Réglages) : enregistrés dans
`~/.config/odysseus/reglages` (« clé = valeur ») et `theme`, relus à chaud par le
compositeur et les applications. Ils priment sur les variables d'environnement
(`ODYSSEUS_SCALE`, `ODYSSEUS_IDLE`, `ODYSSEUS_NIGHT_*`, `XKB_DEFAULT_*`), qui restent
les valeurs par défaut sans fichier de réglages. Thèmes : Odyssée bleue (par défaut),
sombre et claire. Une nouvelle définition d'écran revient d'elle-même à l'ancienne
au bout de 15 s si on ne la garde pas.

Écran : branché à chaud (ODYSSEUS_OUTPUT est l'écran préféré, sinon le premier
connecté). Par IPC (`SWAYSOCK`) : `output scale 1.25`, `output mode 2560x1440@60`,
`nightlight on|off|auto|toggle` (auto : 21 h → 7 h, `ODYSSEUS_NIGHT_FROM/TO/TEMP`).

Fichiers et Aperçu : Espace (aperçu rapide d'une image), Suppr (corbeille, au
format freedesktop), Ctrl+Z (annuler). Fichiers : clic sur un en-tête (trier,
second clic pour inverser), clic sur un élément (panneau d'informations : copier
le chemin, copier l'image, ouvrir, décompresser, terminal, corbeille), Ctrl+C (copier
le chemin), Ctrl+I (panneau), Ctrl+T (terminal dans le dossier affiché), double-clic sur
une archive (décompression à côté : zip, tar, gz, xz, bz2, zst), glisser un élément vers une autre fenêtre (un terminal reçoit son
chemin) ou sur un dossier (déplacement). Aperçu : ← → (image précédente / suivante),
molette (zoom), 0 / 1 (ajuster / taille réelle), R (pivoter), C puis Entrée
(recadrer), I (infos), F (plein écran), Ctrl+S / Ctrl+Maj+S (enregistrer / une copie).

Presse-papiers : `wl-copy` / `wl-paste` (protocole data-control) ; une capture
copiée se colle telle quelle, par exemple dans Claude Code avec Ctrl+V.

Captures d'écran : enregistrées dans `~/Images` (« Capture d’écran AAAA-MM-JJ à HH.MM.SS.png »),
copiées dans le presse-papiers et annoncées par une notification avec vignette (clic : Aperçu).

Clavier Apple (fabricant USB 05ac) : branché, la disposition passe en Français (Mac) ;
retiré, elle revient au PC. Variables : `XKB_DEFAULT_LAYOUT`, `XKB_DEFAULT_VARIANT`, `XKB_DEFAULT_OPTIONS`.

Animations : `ODYSSEUS_ANIM_SLOW=10` les ralentit toutes dix fois (pour les régler à l'œil).

Inactivité (Réglages > Énergie) : l'écran s'éteint au bout de 10 min et la session se
verrouille, puis, si demandé, l'ordinateur se met en veille ; une vidéo en lecture
l'empêche.

## Commandes

    ./odysseus-session.sh          # session complète sur un tty (lancée d'office sur tty1, sway en secours sur tty2)

    odysseus-notify --send "Titre" "Texte"   # notification d'essai
    odysseus-notify --dnd toggle             # « Ne pas déranger »
    odysseus-control --vue wifi              # centre de contrôle, vue Wi-Fi
    odysseus-notify --center [show]          # centre de notifications (bascule, ou ouvre)
    odysseus-moniteur                        # moniteur système (bascule)
    odysseus-reglages [ecran|fond|clavier…]  # Réglages, sur une page donnée

    cargo build --release          # tout compiler
    cargo run -p shell-bar         # lancer la barre (depuis un terminal dans sway)
    cargo clippy --workspace       # vérifications

Pour lancer la barre au démarrage de sway, ajouter dans `~/.config/sway/config` :

    exec ~/odysseus-desktop/target/release/shell-bar

## Agents (MCP)

`odysseus-mcp` parle le Model Context Protocol sur l'entrée et la sortie standard. Pour
le brancher sur Claude Code (une fois) :

    claude mcp add odysseus -s user -- ~/odysseus-desktop/target/release/odysseus-mcp

Catégories, réglées dans Réglages > Agent (ou dans `~/.config/odysseus/agent.json`, clé
`autorisations`) : lire l'état du système, voir le bureau
(permises par défaut) ; voir l'écran, agir sur le bureau, presse-papiers (refusées par
défaut). Un refus est expliqué à l'agent ; chaque appel est consigné dans
`~/.local/state/odysseus/agent.log` et affiché dans Réglages. Pendant une capture, un œil
rouge s'allume dans la barre. Lancé par un agent qui tourne en root, le serveur prend
l'identité de l'utilisateur du bureau. Le compositeur offre pour cela, par IPC, la liste
des fenêtres (`GET_WINDOWS`, 100), `focus <id>` et `agent-capture <fichier> [<id>]`.

Agent local : Réglages > Agent installe Ollama (archive officielle, empreinte SHA-256
vérifiée, dans `~/.local/opt/ollama`, service utilisateur systemd ; bibliothèques NVIDIA
retirées sur une carte AMD) et télécharge les modèles du catalogue de `agent.json`
(clé `modeles_conseilles`, à compléter librement). Sur une carte AMD, le calcul passe par
Vulkan (Mesa RADV) : `/dev/dri/renderD*` doit appartenir au groupe `render` (règle udev
de systemd ; le groupe doit exister).

## Dépendances système

Au-delà de Smithay et des bibliothèques Wayland : PipeWire et WirePlumber (son),
iwd (Wi-Fi), BlueZ (Bluetooth), logind et polkit (veille, arrêt), PAM (verrou),
wl-clipboard (`wl-copy` / `wl-paste`), librsvg (icônes SVG), hwdata (nom de la carte
graphique). Températures : `k10temp` (processeur AMD) et `amdgpu`.

## Licence

MIT — © 2026 David Courtin. Voir [LICENSE](LICENSE). Le compositeur est parti de
*smallvil* (Smithay, MIT) : voir `compositor/LICENSE-smithay.txt`.
