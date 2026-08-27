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


## Fase 1 UITSLAG (2026-08-27, runs ux1-ux4 op de publieke image, bewijs in shots)

EERSTE VONDST, KRITIEK, direct gefixt en geherpubliceerd (a8416e3): de zojuist
gepubliceerde image PANIKTE bij boot zodra QEMU's default-NIC aanwezig was —
de [io5]-SMB-selftest belde ongevraagd 10.0.2.2:445 (de host van de gebruiker!)
en de parser crashte op een kort antwoord. SMB-parser nu bounds-safe, net-
selftests dev-only, downloads+live opnieuw uitgerold.

| App | opent | sluit/drag | lezen | maken/bewerken/opslaan | oordeel |
|---|---|---|---|---|---|
| EuroFiles | ✓ | drag ✓ | navigatie sidebar+mappen ✓ | n.v.t. (open-met nog te toetsen) | goed |
| EuroNotes | ✓ | ✓ | note-selectie ✓ | **NEE: read-only, typen wordt stil geslikt, geen Nieuw** | **hoofdgat** |
| EuroClock | ✓ | ✓ | RTC + wereldklokken ✓ | n.v.t. | goed |
| EuroWeb | ✓ | ✓ | adresbalk-invoer ✓, eerlijke foutmelding | n.v.t. | redelijk (startpagina leeg) |
| Terminal | ✓ | ✓ | shell werkt | ✓ | goed |
| EuroBeheer | ✓ | ✓ | EuroGuard-status/policy/audit live ✓ | domain-block-input te toetsen | goed |
| Store/Star/Monitor/Log | ✓ | ✓ | live data ✓ | n.v.t. | goed |
| EuroText | ✓ | ✓ | ✓ | **JA: typen ✓, Open/Save-knoppen, 'unsaved changes'-status** | goed |
| Venstersysteem | | drag ✓ focus ✓ snap ✓ max/min-knoppen aanwezig | | vrije resize ontbreekt | redelijk |

## Fase 2 fix-volgorde
1. **EuroNotes bewerkbaar**: nieuwe note (+knop), typen in de geopende note,
   opslaan naar EuroFS (/home/<user>/notes/), verwijderen. Patroon: EuroText.
2. Stille toets-inslik verbieden: een app die invoer niet aankan toont dat.
3. Files: Enter/dubbelklik opent bestand in EuroText (via euromime).
4. Venster-resize (greep rechtsonder).
5. EuroWeb-startpagina: minimale welkomstpagina i.p.v. leeg wit.
