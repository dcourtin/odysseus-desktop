#!/usr/bin/env python3
"""Icônes des applis d'odysseus : des symboles courants, simples, sur une même
tuile. Chaque appli a sa couleur (un léger dégradé, plus clair en haut) et un
dessin blanc, lisible jusqu'à la taille de la roue et de la barre.

  Fichiers   dossier
  Notes      bloc-notes (tuile claire, bandeau jaune)
  Réglages   roue dentée
  Aperçu     photo (montagne et soleil)
  Assistant  bulle de dialogue

Lancer depuis la racine du dépôt : python3 tools/icones.py
"""

import math

GABARIT = """<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128">
  <!-- Icône d'odysseus (tools/icones.py) : {nom}, {objet} -->
  <defs>
    <linearGradient id="fond-{id}" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="{haut}"/>
      <stop offset="1" stop-color="{bas}"/>
    </linearGradient>
  </defs>
  <rect x="8" y="8" width="112" height="112" rx="28" fill="url(#fond-{id})"/>
{dessin}
  <rect x="8.5" y="8.5" width="111" height="111" rx="27.5" fill="none" stroke="#000000" stroke-opacity="0.18"/>
</svg>
"""

BLANC = "#ffffff"


def dossier():
    return """  <path d="M30 42a8 8 0 0 1 8-8h16l8 8h28a8 8 0 0 1 8 8v4H30z" fill="#cfe1ff"/>
  <rect x="30" y="50" width="68" height="46" rx="8" fill="#ffffff"/>"""


def bloc_notes():
    return """  <path d="M8 36a28 28 0 0 1 28-28h56a28 28 0 0 1 28 28v6H8z" fill="#f2c14e"/>
  <path d="M30 60h68M30 76h68M30 92h44" stroke="#cfc6b6" stroke-width="5" stroke-linecap="round"/>"""


def roue_dentee():
    # Huit dents trapézoïdales autour d'un disque, trou au centre
    cx = cy = 64
    r_out, r_in, n = 38, 29, 8
    pts = []
    for k in range(n):
        a = 2 * math.pi * k / n
        for da, r in ((-0.20, r_in), (-0.13, r_out), (0.13, r_out), (0.20, r_in)):
            pts.append((cx + r * math.cos(a + da), cy + r * math.sin(a + da)))
    d = "M" + "L".join(f"{x:.2f} {y:.2f}" for x, y in pts) + "z"
    hole = f"M{cx + 12} {cy}a12 12 0 1 0 -24 0a12 12 0 1 0 24 0z"
    return f"""  <path d="{d}{hole}" fill="{BLANC}" fill-rule="evenodd" stroke="{BLANC}" stroke-width="3" stroke-linejoin="round"/>"""


def photo():
    return """  <rect x="28" y="34" width="72" height="60" rx="10" fill="#ffffff"/>
  <circle cx="81" cy="52" r="7" fill="#f2c14e"/>
  <path d="M34 88l20-24 14 16 8-9 18 17z" fill="#2f9a8e" stroke="#2f9a8e" stroke-width="2" stroke-linejoin="round"/>"""


def bulle():
    return """  <path d="M64 30c21 0 36 12 36 28S85 86 64 86c-4 0-8-.4-11.5-1.3L38 96l3.5-15C33 76 28 68 28 58c0-16 15-28 36-28z" fill="#ffffff"/>
  <circle cx="50" cy="58" r="5" fill="#c9653f"/>
  <circle cx="64" cy="58" r="5" fill="#c9653f"/>
  <circle cx="78" cy="58" r="5" fill="#c9653f"/>"""


# Chemin, nom, objet, couleur du haut et du bas de la tuile, dessin
ICONES = [
    ("files/data/odysseus-files.svg", "Fichiers", "dossier", "#5c9cf2", "#3a73d1", dossier),
    ("notes/data/odysseus-notes.svg", "Notes", "bloc-notes", "#fdfaf3", "#efe9dc", bloc_notes),
    ("settings/data/odysseus-reglages.svg", "Réglages", "roue dentée", "#9a958f", "#6d6863", roue_dentee),
    ("preview/data/odysseus-apercu.svg", "Aperçu", "photo", "#4cc2b3", "#2a9488", photo),
    ("assistant/data/odysseus-assistant.svg", "Assistant", "bulle de dialogue", "#e8906d", "#c9653f", bulle),
]

for chemin, nom, objet, haut, bas, dessin in ICONES:
    with open(chemin, "w") as f:
        # Identifiant propre à chaque icône : plusieurs peuvent cohabiter dans un document
        ident = chemin.rsplit("/", 1)[-1].removesuffix(".svg")
        f.write(GABARIT.format(id=ident, nom=nom, objet=objet, haut=haut, bas=bas, dessin=dessin()))
    print(chemin)
