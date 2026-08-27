# Chromium op EuroOS: van eerste pixel tot echt surfen

*Status per 2026-08-26. Alle claims in dit document zijn gemeten; de
bewijs-screenshots staan in `docs/proof/`, de logbestanden en methodiek in de
sprintdocumenten (`SPRINT-PLAN-CHROME-*.md`).*

## Wat werkt, met bewijs

**Een echte, ongewijzigde Chromium 152 (485 MB Linux-binary) draait als app op
EuroOS** en heeft op 2026-08-26 de echte site van het project geladen:

- `https://euro-os.eu/nl/` (over http, zie Beperkingen) — de volledige
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

## De architectuur in vijf regels

1. **Weergave**: chrome tekent via de in-kernel X-server (`kernel/src/xserver.rs`);
   vensters worden met één gedeelde transform geplaatst (popups op hun echte
   positie).
2. **Input**: pagina-input loopt over de **DevTools-inputbrug** — chrome start
   met `--remote-debugging-pipe`, de kernel attacht (input-only) en vertaalt
   muis/toetsen naar `Input.dispatchMouseEvent`/`insertText`. Die route werkt
   in élke pump-toestand; X-events blijven de route voor browser-UI (menu).
   Achtergrond: of chrome's UI-thread zijn X-socket ooit poll't hangt af van
   een glib-context-race bij startup die van buitenaf niet te sturen is.
3. **Naamresolutie**: glibc's resolver valt terug op 127.0.0.1:53 — dus IS
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

- **https**: de TLS-handshake (BoringSSL) haalt chrome's timeout niet onder
  de ~60× vertraagde emulatie zonder KVM; de TCP-verbindingen naar :443 komen
  wél tot stand. Op echte hardware of met KVM verdwijnt dit. Voor de test
  serveert nginx de site over http aan uitsluitend het VM-adres
  (`conf.d/euroos-vm-test.conf` + het :80-blok); echte bezoekers behouden de
  https-redirect. **Terugdraaien zodra TLS rond is.**
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
