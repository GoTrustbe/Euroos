# Sprint: DE KLIK AF — chrome volledig interactief op EuroOS

Doel (einddefinitie, meetbaar): een muisklik in het chrome-venster verandert
aantoonbaar de browser-UI (before/after-screenshot verschilt op de juiste
plek), en toetsaanslagen landen in de omnibox. Daarna: hetzelfde op de live
desktop (chrome als venster naast de EuroOS-apps).

## Stand (gemeten, run cen1)
- Browser gezond met vDSO: paint-lus 30/s, 0 aborts.
- Events bereiken chrome en worden GELEZEN (conn0 leest na de klik, 6x).
- Na de klik: 2x QueryPointer (op=38) — chrome's input-pijplijn verwerkt iets.
- Nog onbewezen: een ZICHTBAAR gevolg van de klik.

## Stappen
1. BEWIJS-RUN: klik op de tabbalk (scherm 852,260 = venster 291,19) en in het
   paginagebied, met shots vlak voor en 10 s na elke klik. Verschillende
   md5's op de klikplek = de mijlpaal. [decisive]
2. Als shots identiek: het kleine verschil zoeken — welke van de drie
   afleverwegen (eigenaar conn0 / reader-rewrite conn1-3) triggerde de
   QueryPointers; die weg isoleren en de andere twee uitzetten (ruis weg).
3. Motion repareren: cen1 toont kind=2/3 (KEY-events!) bij 'move' — de
   QMP-tablet-motion wordt ergens als toets vertaald. Chrome hovert dus nooit.
   MotionNotify (kind=6) moet uit een move komen. [motion]
4. Toetsenbord: klik in omnibox → typ "euro-os.eu" → shot toont tekst.
   Keycode-map be-azerty al aanwezig in qmp-input.py. [type]
5. Desktop-run: `chrome` op de live desktop, klik via de echte muis-route
   (deliver_to_shown), zelfde bewijslast. [desktop]
6. Consolidatie: tests groen (host), docs, memory, commit per stap.

## Werkwijze (les van gisteren)
- Harness wacht niet meer op eurovnc (gefixt, 614631a).
- ELKE run beantwoordt ALLE openstaande vragen van dat moment (traces aan).
- Geen run zonder before/after-shots: alleen pixels bewijzen interactie.
