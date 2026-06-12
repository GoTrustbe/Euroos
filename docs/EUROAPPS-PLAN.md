# EuroOS — Sprint AC: EuroApps (the missing desktop apps)

*What people expect on a desktop OS, what EuroOS already has, what's still missing.*

**Status:** ⬜ planned
**Priority:** high — a desktop without these isn't usable by non-technical people
**Kind:** mix of `N 🧪` (host-testable cores) + `🔒` (compositor-attended GUI)
**Source:** user-supplied `EUROSUITEAPPS1.md` (2026-06-06).

EuroOS today has: Terminal (shell-in-compositor), **EuroSuite Writer/Calc/Impress GUI**
(BB-5, Word/Excel/PowerPoint-style), EuroAgent, coreutils, EuroLocale (24 langs),
accessibility, a compositor with windows. This sprint fills the everyday-app gap.

Every app follows the house pattern: **pure logic in a host-tested crate**, then a thin
compositor/`suite_ui`-style renderer. Each carries a **sovereign differentiator** no
mainstream OS has, because the kernel primitives (EuroGuard caps, Ed25519, EuroFS
immutability, TPM, audit log) are already there.

---

## Category 1 — Essential (expected day 1)

| # | App | What | Sovereign angle | Complexity | Reuses |
|---|-----|------|-----------------|------------|--------|
| 1 | **EuroFiles** | Graphical file manager: create/copy/move/delete, drag&drop, search, bookmarks | Immutability 🔒 badges, per-file capability labels, EuroSnap restore points on right-click | Medium | compositor + EuroFS |
| 2 | **EuroView** | Viewer for PDF, .docx/.odt, images | Uses EuroDoc model for office docs; **sovereign PDF parser** (no PDFium) | Medium | eurodoc, eurodocio |
| 3 | **EuroShot** | Screenshot: full/window/region, annotate, save/clipboard, delay | Optional Ed25519 on metadata ("unmodified, taken at \<ts\>") | **Low** | compositor framebuffer + eurotls |
| 4 | **EuroMedia** | Image viewer: PNG/JPEG/WebP/SVG/GIF, prev/next, zoom, rotate, EXIF | EXIF location shown only after explicit consent (privacy-first) | Low | image decode |
| 5 | **EuroClip** | Clipboard manager: history, search, pin, auto-clear | GDPR-native: history never hits disk unless pinned; EuroVault-recognised passwords excluded | Low | compositor focus events |

## Category 2 — Expected within the first week

| # | App | What | Sovereign angle | Complexity | Reuses |
|---|-----|------|-----------------|------------|--------|
| 6 | **EuroClock** | Clock/world-time, alarm, stopwatch, timer | EuroLocale 24h/12h per locale; basis for EuroAgent meeting triggers | Low | eurolocale, euroaudio |
| 7 | **EuroReken** | Calculator (std/scientific/programmer), history, unit convert. *(Renamed from "EuroCalc" to avoid clash with Calc spreadsheet)* | Programmer mode hex/oct/bin | **Low** | **reuses eurocalc formula engine** |
| 8 | **EuroNotes** | Notes with Markdown, tags, search, pin, colour | Signed EuroFS files; optional `APPEND_ONLY` tamper-evident notes | Low | eurofs |
| 9 | **EuroArchive** | .zip/.tar/.tar.gz/.tar.zst create/extract/view, password | Verifies Ed25519 sigs if present in manifest | Low | zstd/miniz |
| 10 | **EuroFont** | Font manager: view/install TTF/OTF, preview, activate | Fonts signed as `.eupkg` for sovereign distribution | Low | AA font rasterizer |

## Category 3 — Expected for serious use

| # | App | What | Sovereign angle | Complexity | Reuses |
|---|-----|------|-----------------|------------|--------|
| 11 | **EuroMail** | IMAP/SMTP, HTML+text, attachments, S/MIME or PGP, multi-account, offline cache | EuroCA issues S/MIME certs; EuroVault stores passwords; optional GDPR audit log | High | euronet, eurotls, eurovault, euroca |
| 12 | **EuroContacts** | vCard address book, groups, import/export | CardDAV sync via own server (no Google Contacts) | Low-Med | — |
| 13 | **EuroCalendar** | Month/week/day, events, recurrence, reminders | EuroAgent `calendar_read` MCP backend; EuroLocale week-start (Mon in EU) | Medium | eurolocale, euroagent |
| 14 | **EuroMusic** | MP3/FLAC/OGG, playlists, cover art, shuffle | No streaming telemetry | Medium | euroaudio |
| 15 | **EuroCapture** | Screen recording to MP4/WebM, mic optional, region | Native framebuffer access via compositor | Medium | compositor, euroaudio |
| 16 | **EuroVideo** | MP4/MKV/WebM, subtitles, speed, fullscreen | Soft-decode first (GPU later K4) | High | euroaudio |

## Category 4 — Sovereign differentiators (unique to EuroOS) ⭐

| # | App | What | Why unique | Complexity | Reuses |
|---|-----|------|------------|------------|--------|
| 17 | **EuroSafe** | Privacy dashboard: which apps hold which capabilities, one screen to manage all permissions, recent audit events, active agents+caps | No mainstream OS shows a **realtime kernel capability view**. The visible face of EuroGuard. | **Low** (data exists) | euroguard, europol, P3 audit |
| 18 | **EuroHealth GUI** | Graphical `eurohealth`: SMART, EuroFS integrity, mem/CPU, history, proactive warnings | Disk + FS integrity + security health on one screen — Task Manager / Activity Monitor don't | Low | eurohealth, euroobserve |
| 19 | **EuroVPN GUI** | WireGuard profiles, connect/disconnect, status, logs | VPN built into the OS, not an add-on | Low | eurovpn |
| 20 | **EuroSign** | Sign files with Ed25519 (EuroVault key), verify, visual PDF signature, export cert | Sovereign document signing, no cloud/paid service — relevant to gov/notary/legal | Low-Med | eurotls, eurovault, euroca |

---

## Build order (by value × low complexity, reusing existing infra first)

```
Fase 1 — day-1 usable (all low/medium):
  EuroShot → EuroReken → EuroSafe → EuroNotes → EuroMedia → EuroFiles
Fase 2 — first week complete:
  EuroView → EuroClip → EuroClock → EuroFont → EuroArchive
Fase 3 — serious use:
  EuroCalendar → EuroContacts → EuroMusic → EuroCapture
Fase 4 — full platform:
  EuroMail → EuroVideo → EuroVPN GUI → EuroSign → EuroHealth GUI
```

**Rationale for the Fase-1 reordering vs the source doc:** lead with the apps that reuse
existing host-tested cores and ship a sovereign hook with near-zero new infrastructure —
**EuroReken** (eurocalc engine), **EuroSafe** (euroguard/audit data already exists),
**EuroShot** (compositor framebuffer + Ed25519). These prove the pattern fastest.

---

*Claude Code build commands: `"bouw EuroShot"` · `"implementeer EuroReken"` ·
`"start EuroSafe capability dashboard"`. Each = a host-tested core crate + a `suite_ui`-style
compositor renderer + a boot self-test, same as BB-5 EuroSuite.*
