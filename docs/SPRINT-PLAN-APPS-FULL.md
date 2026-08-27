# Sprint: apps naar VOLWAARDIG niveau (geen MVP)

Eigenaar (2026-08-27): geen MVP-kwaliteit, breder en uitgebreider denken, voor
ALLE apps. Elke app moet bij oplevering aanvoelen als een echt product, niet als
een demo die later "beter" moet.

## De lat per app (wat een gebruiker écht verwacht)

### EuroPaint (eerst — de concrete klacht: 12 kleuren)
- Volledige HSV-kleurkiezer (hue-strip + SV-veld) + RGB-hex invoer, niet 12 swatches.
- Recente-kleuren-rij; voorgrond/achtergrond-kleur.
- Gereedschap: penseel, potlood, lijn, rechthoek (kader+gevuld), ellips,
  emmer (flood fill), pipet (kleur oppikken), gum, tekst? (later).
- Instelbare penseelgrootte via slider (1..64), niet 4 vaste.
- Undo/redo (geschiedenis-stack).
- Nieuw canvas met kiesbare grootte; Opslaan-als PNG/QOI/BMP met bestandsnaam.
- Openen van bestaande afbeelding om te bewerken.
- Zoom/pan zou fijn zijn (later als het te groot wordt).

### EuroView
- Zoom (fit / 100% / in-uit), volgende/vorige in de map, roteren/spiegelen,
  EXIF/afmetingen/bestandsgrootte, achtergrond wisselen.

### EuroText
- Cursor + selectie + invoegen MIDDEN in de tekst (nu alleen aan het eind!),
  regelnummers, zoeken, knippen/plakken, Opslaan-als.

### EuroNotes
- Cursor-bewerking (idem), notities hernoemen/verwijderen, zoeken op tag/tekst,
  live Markdown-preview naast de bron.

### EuroFiles
- Kopiëren/knippen/plakken/hernoemen/verwijderen/nieuwe map, eigenschappen,
  meerdere selectie, sorteren, thumbnails voor afbeeldingen.

### EuroReken (calculator)
- Wetenschappelijke functies, geheugen (M+/M-/MR), geschiedenis.

## Aanpak
Per app: eerst het volledige ontwerp, dan bouwen tot dat niveau, host-getest +
boot-bewezen. Beginnen met EuroPaint als vlaggenschip van "volwaardig".
Gemeenschappelijke basis eerst: een echte HSV-kleurkiezer-widget en een
tekst-editing-core met cursor (die EuroText én EuroNotes delen).
