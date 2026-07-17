# Sprint: Interactive toolkit apps on EuroOS

Goal: turn the proven "live interactive GTK window on the desktop" foundation into
real, usable toolkit-app support — keyboard input, window lifecycle, and toolkit
breadth (SDL) — so EuroOS hosts genuine desktop applications, not just one demo.

Baseline (done, branch feature/app-control, commit c426f13): a live GTK3 app runs as a
framed window on the desktop, renders (shapes + text via image-fallback, prebuilt
fontconfig cache), and a click activates its button.

## Legs (execute back-to-back, commit after each)

- **Leg A — Keyboard to the focused X window.** Route PS/2 scancodes to the hosted X
  app when its window is focused (pump_keyboard by focus, not fullscreen). A GTK app
  with a GtkEntry echoes typed text. Verify with injected scancodes → text appears in
  the window.

- **Leg B — Window lifecycle for the hosted app.** Closing the X-client window (title-bar
  close) must terminate the glibc app cleanly and free its arena/PML4 (spawn_glibc_
  persistent currently leaks). No crash; RAM recovers. Optionally relaunch from a dock
  tile.

- **Leg C — Toolkit breadth: SDL2.** Get a real SDL2 app rendering to a window via the
  in-kernel X server (SDL uses X11 + a software framebuffer → XPutImage, the path that
  already works). Proves the foundation is not GTK-specific.

- **Leg D (stretch) — A polished real GTK app** worth keeping in the dock (e.g. a live
  system-y widget or a small editor), as the showcase deliverable.

## Method
Per leg: implement, build (`./scripts/build.sh release`), boot headless
(`qemu-system-x86_64 -m 1024M ... -serial file -qmp unix:sock`), verify via serial
markers + a QMP screendump, commit (NO Claude trailers, do not push). Roll straight
into the next leg. Update memory (eurokernel-glibc-chromium.md) at the end.
