# Chromium op EuroOS: van eerste pixel tot echt surfen

*Status per 2026-09-04. Alle claims in dit document zijn gemeten; de
bewijs-screenshots staan in `docs/proof/`, de logbestanden en methodiek in de
sprintdocumenten (`SPRINT-PLAN-CHROME-*.md`).*

## Wat werkt, met bewijs

**Een echte, ongewijzigde Chromium 152 (485 MB Linux-binary) draait als app op
EuroOS** en heeft op 2026-08-26 de echte site van het project geladen:

- `https://euro-os.eu/nl/` (over http, zie Beperkingen): de volledige
  Nederlandse homepage, opgemaakt met CSS, twee webfonts, afbeeldingen en
  werkende JavaScript (de cookiebanner van de site verschijnt). Bewijs:
  `proof/2026-08-26-euro-os-eu-nl-rendered.png` en de request-waterval in het
  nginx-logboek van euro-os.eu (HTML → euro.css → desktop.png 176 KB →
  inter-var.woff2 → jetbrains-mono-var.woff2 → manifest.webmanifest).
- Interactie: klikken wordt door de pagina verwerkt (JS-handler, banner),
  klik-op-link navigeert (tab-titel, adresbalk en inhoud wisselen), typen
  landt in een invoerveld met knipperende cursor, en chrome's eigen ⋮-menu
  opent op een klik. Bewijs: de vier click/typ/menu-screenshots in `proof/`.
- Desktop: `chrome` typen in de EuroOS-Terminal start Chromium in een
  EuroOS-venster met EuroGuard-badge; EuroGuard blokkeert live het
  tracker-verkeer van de browser. Bewijs: `proof/2026-08-25-chrome-desktop-app.png`.

De volledige netwerkketen is van het project zelf: EuroOS beantwoordt DNS op
zijn eigen loopback (met EuroGuard-DNS-filtering), de from-scratch TCP-stack
draagt het verkeer, de virtio-net-driver praat met de NIC. glibc, X11, input,
demand-paging: allemaal de eigen kernelimplementaties.

## Op echte hardware, over https (2026-09-03)

Alles hierboven is onder emulatie gemeten. Sinds 3 september draait het op een
Intel N100 met KVM, en dat verschuift twee dingen van "in principe" naar
"gemeten":

- **Chromium schildert en levert een frame af op echte hardware.** Een
  volledige pagina, 800x600, via de kernel-CDP-pomp naar buiten gebracht.
  Bewijs: `proof/2026-09-02-first-frame-on-real-hardware.png`. Wat het
  ontgrendelde: V8's pointer-compressie eist een cage-basis op 4 GiB, en wij
  lijnden grote reserveringen uit op hun eigen grootte. De renderer zat in een
  zichtbare lus (1 GiB mappen, benoemen, unmappen, opnieuw).
- **De echte site rendert over https.** `https://euro-os.eu/` volledig
  opgemaakt. Bewijs: `proof/2026-09-02-live-website-over-https.png`. De
  beperking hierboven ("https haalt de timeout niet") is daarmee vervallen,
  maar niet om de reden die daar stond: het lag niet aan snelheid.

### Waarom https eerder niet werkte

De TLS-handshake stopte precies na de eerste serverflight: chrome schreef
niets meer, las niets meer en vroeg de socket zelfs niet meer op. Dat is waar
certificaatverificatie wacht, en chrome zei het zelf in zijn log:

    Error initializing NSS: libsoftokn3.so: cannot open shared object file

Twee oorzaken, beide gerepareerd:

1. **NSS ontbrak in de image.** NSS laadt zijn softwaretoken en vertrouwde
   wortelcertificaten als losse bibliotheken tijdens de uitvoering, dus ze
   staan niet in de afhankelijkhedenlijst die een linker rapporteert.
   `scripts/mk-nss-pack.sh` bouwt een kleine extra EuroPack-schijf; de kernel
   scant elke schijf op pack-volumes, dus verdere bedrading is niet nodig.
2. **`/dev/urandom` bestond alleen als chrome vanaf de desktop startte.** NSS
   zaait zijn RNG daaruit; mislukt dat, dan meldt het `CKR_DEVICE_ERROR` en
   niets meer. De tekenapparaten worden nu geregistreerd voordat er ook maar
   een programma draait, en ze zijn echt: leesacties worden gegenereerd en
   houden nooit op. Voorheen waren het bestanden van 4 KB, dus een lezer kreeg
   na 4096 bytes "einde bestand", en dat kent een tekenapparaat niet.

Zelftest `gnss` reproduceert de hele storing in een klein programma, zodat een
antwoord een boot kost in plaats van een browserrun.

### Multiproces: opgelost (2026-09-03)

De blokkade is gevonden en zat in een sleutel. Descriptors die onderweg
waren, stonden opgeslagen onder het fd-nummer van de ontvanger. Dat breekt
zodra een socket-uiteinde wordt gedupliceerd of aan een ander proces wordt
doorgegeven: dezelfde verbinding heeft dan meerdere nummers. En de verzender
zocht zijn tegenpartij in de tabel van nummers van het oorspronkelijke
socketpaar, dus een socket die zelf via SCM_RIGHTS was binnengekomen had geen
tegenpartij en de descriptor verdween geruisloos.

Dat is precies wat mojo doet zodra twee processen aan elkaar zijn
voorgesteld: de makelaar geeft elk kind een uiteinde van een socket, en vanaf
dan geven ze elkaar handles rechtstreeks door. Al die handles verdwenen,
waardoor geen dataring (gedeeld geheugen dat beide kanten moeten mappen) ooit
tot stand kwam: aanvragen bereikten de netwerkdienst wel (die lopen via de
makelaar), antwoordlichamen bereikten de renderer nooit.

Descriptors zijn nu gesleuteld op de verbindingszijde waarvoor ze bestemd
zijn (`euronet::unix::Endpoint::key`), wat dup en doorgifte overleeft.
Zelftest `gscm3` dekt het af: A stuurt een memfd over de socket die de
makelaar hem gaf, B mapt hem en leest A's bytes.

Resultaat, zelfde pagina, zelfde hardware:
`proof/2026-09-03-out-of-process-renderer-paints-live-site.png`: euro-os.eu
volledig getekend door een out-of-process renderer, over TLS, met webfonts,
stylesheet en cookiebanner. Voor en na, per teller: responses 2 naar 5,
blink Paint 0 naar 9, Layout 0 naar 18, BeginMainFrame 0 naar 5, RasterTask
0 naar 3, tracing-acks: geen time-out meer.

Nog een les uit de opname zelf: het eerste screencast-frame komt binnen
terwijl de pagina nog laadt. De pomp bewaart nu het nieuwste frame en
verstuurt het zesde; het verschil was 251 tegen 4201 kleuren.

### De uitsluitingslijst die ernaartoe leidde

Single-process rendert de live site van begin tot eind. Met een
out-of-process renderer krijgt die zijn document wel (8901 tekens, 2
stylesheets) maar wordt geen enkele subbron afgerond: hij noemt zelf waar hij
op wacht (`euro.css`), de aanvraag gaat de deur uit, de bytes komen terug, en
het antwoord komt nooit af. Blink schildert daarom niet, want een
render-blokkerende stylesheet houdt het eerste frame tegen.

Van onderaf is de lijst inmiddels vrij compleet uitgesloten, elk punt gemeten:

| gecontroleerd | uitkomst |
|---|---|
| TCP-verlies en herstel | 0 gaten na de hertransmissie-fix (was 89 per lading) |
| onopgehaalde bytes in socketbuffers | 0 B: chrome consumeert alles |
| verstuurt chrome de aanvragen | ja, 500-600 B per verbinding |
| zusterprimitieven (socket, gedeeld geheugen, eventfd) | alle drie werkend (`gscm3`) |
| memfd-zegels | echt geimplementeerd, geen verschil |
| descriptors kort na elkaar | was kapot, gerepareerd, geen verschil |
| CPU-verdeling | renderer krijgt 28% |
| vectorregisters over taakwissels | blijven intact (`gvec`) |

De volgende meting hoort binnen chrome's eigen plumbing. Dat vraagt de
renderer-trace, en die bereikt ons om vermoedelijk dezelfde reden niet
(`CrRendererMain=0`): ook de tracebuffer komt van een zusterproces.

## De architectuur in vijf regels

1. **Weergave**: chrome tekent via de in-kernel X-server (`kernel/src/xserver.rs`);
   vensters worden met één gedeelde transform geplaatst (popups op hun echte
   positie).
2. **Input**: pagina-input loopt over de **DevTools-inputbrug**. Chrome start
   met `--remote-debugging-pipe`, de kernel attacht (input-only) en vertaalt
   muis/toetsen naar `Input.dispatchMouseEvent`/`insertText`. Die route werkt
   in élke pump-toestand; X-events blijven de route voor browser-UI (menu).
   Achtergrond: of chrome's UI-thread zijn X-socket ooit poll't hangt af van
   een glib-context-race bij startup die van buitenaf niet te sturen is.
3. **Naamresolutie**: glibc's resolver valt terug op 127.0.0.1:53, dus IS
   EuroOS daar de nameserver (`Sock::LocalDns`): /etc/hosts eerst, dan een
   echte query naar de DHCP-server; EuroGuard filtert.
4. **Sockets**: poll/epoll rapporteren echte readiness voor AF_INET (lezen én
   schrijven), een lege TCP-read is -EAGAIN en 0 betekent uitsluitend EOF,
   getsockname/getpeername leveren de echte four-tuple, TCP_INFO meldt
   ESTABLISHED. Eén centrale RX-demux (`rx_route`) routeert NIC-frames naar
   per-poort-queues zodat parallelle verbindingen elkaars segmenten nooit
   meer opeten.
5. **Tijd**: de testharness draait QEMU met `-icount`, zodat de gastklok de
   emulatiesnelheid volgt en chrome's interne timeouts (idle-sockets ~10 s)
   zich weer normaal gedragen.

## Beperkingen, eerlijk

- **https**: opgelost, zie hierboven. Vereist wel de NSS-schijf
  (`scripts/mk-nss-pack.sh`) naast de chrome-pack. Onder emulatie zonder KVM
  blijft de handshake traag; op echte hardware niet.
- **Multiproces**: opgelost en herhaalbaar: drie opeenvolgende runs op de
  NUC leverden elk zes screencast-frames en een volledig afgeleverde PNG,
  zonder tracing-time-outs. De boottest draait sindsdien multiproces.
- **Snelheid**: ~20 min per HTTP-resource onder icount-TCG. Puur emulatie;
  geen stack-eigenschap.
- **Browser-UI-input** (omnibox, menu) via X werkt alleen wanneer chrome's
  glib-pump de startup-race wint; pagina-input via de brug is betrouwbaar.
- De start-URL van de boot-run staat in `kernel/src/main.rs` (chrome-boot-
  fase); nu `http://euro-os.eu/nl/`.

## Reproduceren

```
# interactieve boot-run (bouwt kernel + boot + stuurt input + screenshots):
printf 'move 960 540\nwait 300\nshot /tmp/run-a.ppm\n' > /tmp/clicks.txt
env PACK=/tmp/chrome-pack2.img CLICKS=/tmp/clicks.txt \
    bash scripts/chrome-ui-input.sh /tmp/run.log
# desktop-run (typt `chrome` in de Terminal):
env PACK=/tmp/chrome-pack2.img DESKTOP_CLICKS=/tmp/clicks.txt \
    bash scripts/chrome-desktop.sh /tmp/desktop.log
```
De CLICKS-file wordt niet meer geconsumeerd; wachten op `took` in de
harness-output. De pack (`/tmp/chrome-pack2.img`, EUROPCK1-formaat) bevat de
chrome-binary + 94 bibliotheken/bestanden en wordt demand-paged geserveerd.

## De tien fixes die het ontgrendelden (chronologisch)

vDSO voor elke klok (de frame-lus-deadlock) · PS/2-routing op statusbit 5 ·
xHCI-polling (MSI-X bleek een per-boot-loterij) · poll() zonder data-blinde
naps · DevTools-inputbrug · kernel-DNS op loopback · socket-readiness in
poll/epoll · EAGAIN-vs-EOF · getsockname/getpeername (dé navigatie-moordenaar)
· centrale RX-demux. Elk met meting-vooraf en bewijs-achteraf in de commits.
