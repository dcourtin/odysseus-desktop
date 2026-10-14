//! Raccourcis clavier d'odysseus-desktop, partagés par l'aide-mémoire
//! (Super+K) et la page Raccourcis de Réglages.

/// (titre de section, [(touches, description)])
pub type Section = (
    &'static str,
    &'static [(&'static [&'static str], &'static str)],
);

pub const LEFT: &[Section] = &[
    (
        "Fenêtres",
        &[
            (&["Super", "Entrée"], "Terminal"),
            (&["Super", "Espace"], "Lanceur"),
            (&["Super", "Q"], "Fermer la fenêtre"),
            (&["Super", "F"], "Agrandir / restaurer"),
            (&["Super", "Tab"], "Fenêtre suivante"),
            (&["Super", "glisser"], "Déplacer"),
            (&["Super", "clic droit"], "Redimensionner (glisser)"),
        ],
    ),
    (
        "Aimantation",
        &[
            (&["glisser vers un bord"], "Moitié, quart ou plein écran"),
            (&["Super", "←"], "Moitié gauche"),
            (&["Super", "→"], "Moitié droite"),
            (&["Super", "↑"], "Plein écran utile"),
            (&["Super", "↓"], "Restaurer"),
        ],
    ),
    (
        "Bureaux",
        &[
            (&["Super", "1…9"], "Aller au bureau"),
            (&["Super", "Maj", "1…9"], "Y envoyer la fenêtre"),
        ],
    ),
];

pub const RIGHT: &[Section] = &[
    (
        "Captures d’écran",
        &[
            (&["Impr. écran"], "Zone (Espace : une fenêtre)"),
            (&["Maj", "Impr. écran"], "Écran entier"),
        ],
    ),
    (
        "Système",
        &[
            (&["Super", "L"], "Verrouiller"),
            (&["Super", "K"], "Aide-mémoire des raccourcis"),
            (&["Super", "A"], "Assistant (agent du bureau)"),
            (&["Volume +", "Volume −"], "Volume"),
            (&["Muet"], "Couper le son"),
            (&["Ctrl", "Alt", "F1…F12"], "Changer de terminal"),
            (&["Super", "Maj", "E"], "Quitter la session"),
        ],
    ),
    (
        "Fichiers et Aperçu",
        &[
            (&["Espace"], "Aperçu rapide d'une image"),
            (&["Suppr"], "Mettre à la corbeille"),
            (&["Ctrl", "Z"], "Annuler la mise à la corbeille"),
            (&["Ctrl", "C"], "Copier le chemin (Fichiers)"),
            (&["Ctrl", "I"], "Panneau d'informations"),
            (&["Ctrl", "T"], "Terminal dans le dossier (Fichiers)"),
            (&["←", "→"], "Image précédente / suivante"),
            (&["R"], "Pivoter"),
            (&["C", "Entrée"], "Recadrer"),
            (&["Ctrl", "S"], "Enregistrer"),
        ],
    ),
];
