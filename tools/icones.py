#!/usr/bin/env python3
"""Icônes des applis d'odysseus : un dessin au trait, de la couleur d'accent du
thème, sur une tuile de la couleur de fond du thème. Toutes les applis
partagent la même tuile et le même trait : c'est l'identité d'odysseus.

  Fichiers   dossier
  Notes      page lignée
  Réglages   roue dentée
  Aperçu     photo (montagne et soleil)
  Assistant  bulle de dialogue

Les couleurs écrites dans le fichier sont celles du thème sombre (accent terre
cuite), pour un affichage hors d'odysseus. Dans odysseus, common::icons les
remplace par celles du thème actif, grâce aux classes (feuille de style passée
à librsvg) :
  .tuile   fond de la tuile        → fond du thème
  .bord    liseré de la tuile      → encre du thème, discrète
  .trait   dessin au trait         → accent
  .plein   petites parties pleines → accent
L'attribut data-odysseus-theme sur la racine signale une icône à recolorer.

Lancer depuis la racine du dépôt : python3 tools/icones.py
"""

import math

FOND = "#262624"
ACCENT = "#d97757"
ENCRE = "#ffffff"

GABARIT = """<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128" data-odysseus-theme="1">
  <!-- Icône d'odysseus (tools/icones.py) : {nom}, {objet} -->
  <rect class="tuile" x="8" y="8" width="112" height="112" rx="28" fill="{fond}"/>
  <rect class="bord" x="8.5" y="8.5" width="111" height="111" rx="27.5" fill="none" stroke="{encre}" stroke-opacity="0.14"/>
  <g class="trait" fill="none" stroke="{accent}" stroke-width="6" stroke-linecap="round" stroke-linejoin="round">
{dessin}
  </g>
{plein}</svg>
"""


def dossier():
    return """    <path d="M30 44a6 6 0 0 1 6-6h16l7 7h33a6 6 0 0 1 6 6v35a6 6 0 0 1-6 6H36a6 6 0 0 1-6-6z"/>
    <path d="M30 56h68"/>""", ""


def page():
    return """    <rect x="36" y="28" width="56" height="72" rx="9"/>
    <path d="M50 50h28M50 64h28M50 78h16"/>""", ""


def roue_dentee():
    # Huit dents trapézoïdales autour d'un disque, moyeu au centre
    cx = cy = 64
    r_out, r_in, n = 36, 27, 8
    pts = []
    for k in range(n):
        a = 2 * math.pi * k / n
        for da, r in ((-0.21, r_in), (-0.13, r_out), (0.13, r_out), (0.21, r_in)):
            pts.append((cx + r * math.cos(a + da), cy + r * math.sin(a + da)))
    d = "M" + "L".join(f"{x:.2f} {y:.2f}" for x, y in pts) + "z"
    return f"""    <path d="{d}"/>
    <circle cx="{cx}" cy="{cy}" r="10"/>""", ""


def photo():
    return """    <rect x="28" y="34" width="72" height="60" rx="9"/>
    <path d="M28 80l22-20 14 14 10-10 26 20"/>""", f"""  <circle class="plein" cx="80" cy="52" r="6.5" fill="{ACCENT}"/>
"""


def bulle():
    return """    <path d="M64 32c20 0 34 11.5 34 26S84 84 64 84c-4 0-7.6-.4-11-1.2L38 94l3.6-14.4C34.5 75 30 67 30 58c0-14.5 14-26 34-26z"/>""", f"""  <circle class="plein" cx="50" cy="58" r="4.5" fill="{ACCENT}"/>
  <circle class="plein" cx="64" cy="58" r="4.5" fill="{ACCENT}"/>
  <circle class="plein" cx="78" cy="58" r="4.5" fill="{ACCENT}"/>
"""


ICONES = [
    ("files/data/odysseus-files.svg", "Fichiers", "dossier", dossier),
    ("notes/data/odysseus-notes.svg", "Notes", "page lignée", page),
    ("settings/data/odysseus-reglages.svg", "Réglages", "roue dentée", roue_dentee),
    ("preview/data/odysseus-apercu.svg", "Aperçu", "photo", photo),
    ("assistant/data/odysseus-assistant.svg", "Assistant", "bulle de dialogue", bulle),
]

for chemin, nom, objet, dessin in ICONES:
    trait, plein = dessin()
    with open(chemin, "w") as f:
        f.write(GABARIT.format(nom=nom, objet=objet, fond=FOND, accent=ACCENT, encre=ENCRE,
                               dessin=trait, plein=plein))
    print(chemin)
