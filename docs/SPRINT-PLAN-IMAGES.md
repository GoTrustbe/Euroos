# Sprint: afbeeldingen bekijken (EuroView) + maken (EuroPaint)

Opdracht eigenaar (2026-08-27): afbeeldingen bekijken in een EIGEN app (niet de
browser), plus een Paint-achtige app om zelf afbeeldingen te maken/bewerken.

## Fase 1: decoders (crate euromedia)
Bestaat: QOI (enc+dec), PPM/PGM (dec). Toevoegen:
- BMP decode (24/32-bit ongecomprimeerd — makkelijk, geen compressie).
- PNG decode (IHDR → IDAT via euroflate::zlib_decompress → unfilter → pixels).
  Baseline: 8-bit RGB/RGBA/grijs, geen interlace. Host-getest tegen echte PNG's.
- PNG encode (voor Paint/screenshots opslaan): IHDR + zlib(IDAT) + IEND.
- JPEG: EERLIJK LATER (baseline DCT/Huffman = weken); viewer meldt "niet
  ondersteund" i.p.v. crashen.

## Fase 2: EuroView (viewer-app)
- Nieuwe SuiteApp::ImageView. Window met de gedecodeerde afbeelding, geschaald
  in het venster (fit), formaat + afmetingen in de titelbalk.
- mime-wiring: dubbelklik op .png/.bmp/.qoi/.ppm in EuroFiles → opent EuroView.
- Dock-tile + launcher-entry.

## Fase 3: EuroPaint (editor-app)
- Nieuwe SuiteApp::Paint. Canvas (RGBA Image), penseel (klik-sleep), kleurenkiezer,
  gum, wissen. Opslaan naar PNG + QOI op EuroFS.
- Openen van een bestaande afbeelding om te bewerken.

## Verificatie
Elke decoder host-getest tegen ECHTE bestanden (via python om test-PNG/BMP te
maken). Apps boot-getest met QMP + screenshot, zoals de UX-sprint. Regel: nooit
een half-werkende decoder als "werkt" presenteren; onhaalbaar formaat eerlijk
melden.
