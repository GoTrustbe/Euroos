# Sprint: kritische UX-audit + fixes van alle desktop-apps

Opdracht van de eigenaar (2026-08-27): met een kritische blik door ALLE apps
heen zoals een echte gebruiker: vensters verplaatsen/sluiten/vergroten, kan ik
in Notes een NIEUWE notitie maken en bewerken, hoe werken Files/Text/Clock/
Monitor/Log/Beheer echt, waar loopt een gebruiker dood. Eerst meten, dan fixen
op volgorde van ernst. Regel: nooit een viewer als editor presenteren.

## Fase 1: audit (code + live boot met QMP-interactie)
Per app scoren op: openen · sluiten · verplaatsen · resizen · inhoud LEZEN ·
inhoud MAKEN/BEWERKEN/OPSLAAN · toetsenbord · feedback (wat zegt de app als
iets niet kan?). Uitkomst: tabel in dit document, eerlijk.

## Fase 2: fixes, zwaarste eerst
Verwachte toppers (hypothese vooraf, te toetsen):
1. EuroNotes is een read-only viewer die zich als notitie-app presenteert:
   nieuwe note + bewerken + opslaan naar EuroFS ontbreken.
2. Vensters: resizen ontbreekt mogelijk; max/min-gedrag toetsen.
3. EuroText: opslaan-flow en nieuw-bestand-flow toetsen.
4. Overal: dode knoppen die niets zeggen wanneer ze niets doen.

## Verificatie
Elke fix boot-getest met QMP-scripts (klik/typ) + screenshot-bewijs, zoals de
chrome-sprint. Host-tests groen houden.
