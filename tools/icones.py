#!/usr/bin/env python3
"""Icônes des applis d'odysseus : céramique grecque, en formes modernes.
Chaque appli est un objet de l'Odyssée réduit à une silhouette pleine, sur un
fond uni tiré des vases antiques (terre cuite, fond blanc, noir, ocre). Les
découpes prennent la couleur du fond ; un liseré sombre détache la tuile.

  Fichiers   amphore (on y rangeait tout)
  Notes      tablette de cire et stylet
  Réglages   lyre (on l'accorde)
  Aperçu     œil de proue (les trières en portaient un, pour voir)
  Assistant  chouette d'Athéna, qui guide Ulysse

Lancer depuis la racine du dépôt : python3 tools/icones.py
"""

GABARIT = """<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128">
  <!-- Icône d'odysseus (tools/icones.py) : {nom}, {objet} -->
  <rect x="8" y="8" width="112" height="112" rx="28" fill="{fond}"/>
{dessin}
  <rect x="8.5" y="8.5" width="111" height="111" rx="27.5" fill="none" stroke="#000000" stroke-opacity="0.2"/>
</svg>
"""

NOIR = "#1d1916"
TERRE = "#d97757"

# Fonds : couleur de la tuile, couleur de la figure
FONDS = {
    "terre": (TERRE, NOIR),
    "blanc": ("#f3eee3", NOIR),
    "noir": ("#3a312c", TERRE),
    "ocre": ("#e3a24f", NOIR),
}


def amphore(f, c):
    return f"""  <path d="M56 37C44 33 37 41 42 53M72 37C84 33 91 41 86 53" fill="none" stroke="{f}" stroke-width="6" stroke-linecap="round"/>
  <rect x="49" y="26" width="30" height="7" rx="3.5" fill="{f}"/>
  <path d="M57 33h14v10C86 47 93 57 93 69c0 13-10 23-21 29H56C45 92 35 82 35 69c0-12 7-22 22-26z" fill="{f}"/>
  <rect x="54" y="98" width="20" height="6" rx="3" fill="{f}"/>
  <path d="M44 68h40" stroke="{c}" stroke-width="5" stroke-linecap="round"/>"""


def tablette(f, c):
    return f"""  <rect x="30" y="28" width="58" height="72" rx="12" fill="{f}"/>
  <path d="M44 50h30M44 64h22" stroke="{c}" stroke-width="6" stroke-linecap="round"/>
  <path d="M100 36L80 92" stroke="{c}" stroke-width="13" stroke-linecap="round"/>
  <path d="M100 36L80 92" stroke="{f}" stroke-width="6" stroke-linecap="round"/>"""


def lyre(f, c):
    return f"""  <path d="M42 32v30a22 22 0 0 0 44 0V32" fill="none" stroke="{f}" stroke-width="10" stroke-linecap="round"/>
  <path d="M36 44h56" stroke="{f}" stroke-width="7" stroke-linecap="round"/>
  <path d="M56 44v38M64 44v40M72 44v38" stroke="{f}" stroke-width="3.5" stroke-linecap="round"/>
  <rect x="52" y="90" width="24" height="8" rx="4" fill="{f}"/>"""


def oeil(f, c):
    return f"""  <path d="M22 64C34 42 50 34 64 34s30 8 42 30c-12 22-28 30-42 30S34 86 22 64z" fill="{f}"/>
  <circle cx="64" cy="64" r="17" fill="{c}"/>
  <circle cx="64" cy="64" r="9" fill="{f}"/>"""


def chouette(f, c):
    return f"""  <path d="M40 40l-2-12 14 8M88 40l2-12-14 8" fill="{f}" stroke="{f}" stroke-width="4" stroke-linejoin="round"/>
  <ellipse cx="64" cy="70" rx="28" ry="32" fill="{f}"/>
  <circle cx="52" cy="60" r="11.5" fill="{c}"/>
  <circle cx="76" cy="60" r="11.5" fill="{c}"/>
  <circle cx="53" cy="61" r="5.5" fill="{f}"/>
  <circle cx="75" cy="61" r="5.5" fill="{f}"/>
  <path d="M64 70l-4.5 4 4.5 6 4.5-6z" fill="{c}"/>"""


ICONES = [
    ("files/data/odysseus-files.svg", "Fichiers", "amphore", "terre", amphore),
    ("notes/data/odysseus-notes.svg", "Notes", "tablette de cire", "blanc", tablette),
    ("settings/data/odysseus-reglages.svg", "Réglages", "lyre", "noir", lyre),
    ("preview/data/odysseus-apercu.svg", "Aperçu", "œil de proue", "ocre", oeil),
    ("assistant/data/odysseus-assistant.svg", "Assistant", "chouette d'Athéna", "blanc", chouette),
]

for chemin, nom, objet, fond, dessin in ICONES:
    tuile, figure = FONDS[fond]
    with open(chemin, "w") as f:
        f.write(GABARIT.format(nom=nom, objet=objet, fond=tuile, dessin=dessin(figure, tuile)))
    print(chemin)
