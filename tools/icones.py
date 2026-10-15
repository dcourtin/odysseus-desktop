#!/usr/bin/env python3
"""Icônes des applis d'odysseus, d'un même gabarit : carré arrondi, dégradé
vertical d'une teinte tirée des fonds d'odysseus, reflet discret en haut,
liseré sombre, symbole Lucide (ISC) blanc au même trait.

Lancer depuis la racine du dépôt : python3 tools/icones.py
"""

GABARIT = """<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128">
  <!-- Icône d'odysseus (tools/icones.py) : {nom} ; symbole Lucide « {lucide} », ISC -->
  <defs>
    <linearGradient id="fond" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="{haut}"/>
      <stop offset="1" stop-color="{bas}"/>
    </linearGradient>
    <linearGradient id="reflet" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#ffffff" stop-opacity="0.22"/>
      <stop offset="0.5" stop-color="#ffffff" stop-opacity="0"/>
    </linearGradient>
  </defs>
  <rect x="8" y="8" width="112" height="112" rx="28" fill="url(#fond)"/>
  <rect x="8" y="8" width="112" height="112" rx="28" fill="url(#reflet)"/>
  <rect x="8.5" y="8.5" width="111" height="111" rx="27.5" fill="none" stroke="#000000" stroke-opacity="0.18"/>
  <g transform="translate(26 26) scale(3.1667)" fill="none" stroke="#ffffff" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
{trace}
  </g>
</svg>
"""

# Teintes : Mer Égée, Aube sur Ithaque, Marbre (graphite), Crépuscule
ICONES = [
    ("files/data/odysseus-files.svg", "Fichiers", "folder-open", "#5fa5dc", "#2b67a6",
     ['<path d="m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2"/>']),
    ("notes/data/odysseus-notes.svg", "Notes", "notebook-pen", "#f2b95e", "#cc7a2b",
     ['<path d="M13.4 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-7.4"/>',
      '<path d="M2 6h4"/>', '<path d="M2 10h4"/>', '<path d="M2 14h4"/>', '<path d="M2 18h4"/>',
      '<path d="M21.378 5.626a1 1 0 1 0-3.004-3.004l-5.01 5.012a2 2 0 0 0-.506.854l-.837 2.87a.5.5 0 0 0 .62.62l2.87-.837a2 2 0 0 0 .854-.506z"/>']),
    ("settings/data/odysseus-reglages.svg", "Réglages", "settings", "#9ba3b4", "#555c6d",
     ['<path d="M9.671 4.136a2.34 2.34 0 0 1 4.659 0 2.34 2.34 0 0 0 3.319 1.915 2.34 2.34 0 0 1 2.33 4.033 2.34 2.34 0 0 0 0 3.831 2.34 2.34 0 0 1-2.33 4.033 2.34 2.34 0 0 0-3.319 1.915 2.34 2.34 0 0 1-4.659 0 2.34 2.34 0 0 0-3.32-1.915 2.34 2.34 0 0 1-2.33-4.033 2.34 2.34 0 0 0 0-3.831A2.34 2.34 0 0 1 6.35 6.051a2.34 2.34 0 0 0 3.319-1.915"/>',
      '<circle cx="12" cy="12" r="3"/>']),
    ("preview/data/odysseus-apercu.svg", "Aperçu", "image", "#a98bdb", "#6549a8",
     ['<rect width="18" height="18" x="3" y="3" rx="2" ry="2"/>', '<circle cx="9" cy="9" r="2"/>',
      '<path d="m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21"/>']),
]

for chemin, nom, lucide, haut, bas, trace in ICONES:
    with open(chemin, "w") as f:
        f.write(GABARIT.format(nom=nom, lucide=lucide, haut=haut, bas=bas,
                               trace="\n".join("    " + t for t in trace)))
    print(chemin)
