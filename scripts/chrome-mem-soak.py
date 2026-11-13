#!/usr/bin/env python3
"""A long browser session for the memory curve: the site matrix AND a YouTube video
that plays, round after round, in one browser session.

This is a driver around scripts/chrome-smoke.py (loaded as a library, not changed):
the boot, the real input over QMP, the consent dialogs ("Reject all"), the site
matrix are that harness's own. What this adds is the loop and its own YouTube step:

  per round: every site of the matrix (the harness asks the kernel for `ramfiles`
             after each, which prints the memory census), then the YouTube step with
             a video that plays for --video-seconds (picture and sound are checked
             by the harness), then a census.

No logins, no accounts. YouTube as a guest: the consent dialog is answered with a
real click; a "Sign in to confirm you're not a bot" page is reported as it is.

  python3 scripts/chrome-mem-soak.py --image eurokernel.img --pack chrome-pack2.img \
      --nss nss-pack-vid.img --kvm --pin 14-15 --mem 8192M --workdir DIR --hours 2

Read the curve with scripts/mem-curve.py DIR/serial.log <this script's output>.
"""
import importlib.util, os, sys, time

_here = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("chrome_smoke", os.path.join(_here, "chrome-smoke.py"))
cs = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cs)


def pills(g, s):
    """Wide one-colour pill buttons on a light page: [(x_centre, y_centre, width, colour)], top to
    bottom then left to right. A pill is a run of 70 to 330 px of one colour that is not near white,
    found again with the same ends some rows further down (its label interrupts the rows between)."""
    x0, y0, x1, y1 = g.content()
    runs = []
    for y in range(y0 + 40, y1 - 4, 3):
        x = x0 + 8
        while x < x1 - 8:
            c = s.px(x, y)
            n = 0
            while x + n < x1 - 8 and s.px(x + n, y) == c:
                n += 2
            if 70 <= n <= 330 and sum(c) < 600:
                runs.append((x, x + n, y, c))
            x += max(n, 2)
    found = []
    for a in runs:
        same = [b for b in runs if b[3] == a[3] and abs(b[0] - a[0]) <= 14 and abs(b[1] - a[1]) <= 14 and 0 <= b[2] - a[2] <= 60]
        top, bottom = a[2], max(b[2] for b in same)
        if 18 <= bottom - top <= 60 and not any(abs(f[0] - (a[0] + a[1]) // 2) < 30 and abs(f[1] - (top + bottom) // 2) < 30 for f in found):
            found.append(((a[0] + a[1]) // 2, (top + bottom) // 2, a[1] - a[0], a[3]))
    return sorted(found, key=lambda f: (f[1] // 30, f[0]))


def white_share(g, s):
    """Share of white pixels over the place of the player (a consent dialog covers it in white)."""
    x0, y0, x1, y1 = g.content()
    pts = [(x, y) for x in range(x0 + 80, x1 - 120, 24) for y in range(y0 + 80, min(y1, y0 + 340), 24)]
    return sum(1 for x, y in pts if sum(s.px(x, y)) >= 740) / max(1, len(pts))


def youtube_play(g, seconds, rnd):
    """Front page, watch page, the consent dialog if there is one (a real click on its left
    button, 'Reject all', checked by the dialog going away), a real click on the player, then
    picture and sound for `seconds`. Every state is saved as a screenshot; nothing is worked
    around: a page that asks to sign in is recorded as it is."""
    rec = g.step
    tag = f"-r{rnd}" if rnd <= 3 else "-last"
    ok, secs, detail = g.navigate("https://www.youtube.com/", "youtube.com")
    g.move(g.w - 4, g.h - 4)
    g.wait_loaded(60)
    s, _, _ = g.settle(40, quiet=2.5, save_as="yt-front" + tag)
    answered = None
    for where in ("front", "watch"):
        if where == "watch":
            ok, secs, detail = g.navigate(cs.VIDEO_URL, "watch")
            g.move(g.w - 4, g.h - 4)
            g.wait_loaded(60)
            g.nap(6.0)
            s, _, _ = g.settle(40, quiet=3.0, save_as="yt-watch" + tag)
            # The consent dialog is drawn over the watch page some seconds after it loaded.
            for _ in range(8):
                if white_share(g, s) > 0.6:
                    break
                g.nap(3.0)
                s = g.shot()
            s.save_png(os.path.join(g.wd, f"yt-watch{tag}-settled.png"))
            rec(f"youtube: the watch page loads (round {rnd})", ok, f"{detail} after {secs:.1f} s")
            if not ok:
                return False
        url = (g.tabs() or [("", "")])[0][0]
        if "consent." not in url and not (where == "watch" and white_share(g, s) > 0.6):
            continue
        # A consent page or dialog: bring its buttons into view, click the left one of the last row.
        x0, y0, x1, y1 = g.content()
        g.move((x0 + x1) // 2, (y0 + y1) // 2 + 40)
        hit, ps = None, []
        for k in range(5):
            g.wheel(10)
            g.nap(1.0)
            s = g.shot(f"yt-consent-{where}{tag}-{k + 1}")
            ps = pills(g, s)
            # The lowest row that has two buttons side by side: 'Reject all' is its left one.
            rows = {}
            for q in ps:
                rows.setdefault(q[1] // 30, []).append(q)
            pairs = [sorted(v) for _, v in sorted(rows.items()) if len(v) >= 2]
            if pairs:
                hit = pairs[-1][0]
                break
        if hit:
            g.click(hit[0], hit[1])
            g.nap(4.0)
            g.move(g.w - 4, g.h - 4)
            s2, _, _ = g.settle(30, quiet=2.5, save_as=f"yt-consent-{where}{tag}-after")
            url2 = (g.tabs() or [("", "")])[0][0]
            gone = "consent." not in url2 and white_share(g, s2) <= 0.6
            answered = gone
            rec(f"youtube: consent dialog on the {where} page answered with a click on its left button, 'Reject all' (round {rnd})", gone,
                f"clicked at {hit[:2]} (a {hit[2]} px pill), {len(ps)} pills seen; afterwards {url2}, {white_share(g, s2):.0%} of the player area white")
            s = s2
        else:
            answered = False
            rec(f"youtube: consent dialog on the {where} page (round {rnd})", False, "no pair of buttons found after 5 wheel steps; see the yt-consent screenshots")
    # The player: top left of the page, 16:9 over most of the width at this window size.
    px0, py0 = g.page(24, 80)
    pw = min(g.xw - 48, 1280)
    player = (px0 + 20, py0 + 20, px0 + pw - 20, py0 + int(pw * 9 / 16) - 60)
    wav = os.path.join(g.wd, "audio.wav")
    g.click((player[0] + player[2]) // 2, (player[1] + player[3]) // 2)      # the user gesture
    g.nap(3.0)
    g.move(g.w - 4, g.h - 4)
    wav0 = os.path.getsize(wav) if os.path.exists(wav) else 0
    t0 = time.time()
    digests, shots = [], 0
    while time.time() - t0 < seconds:
        s = g.shot()
        digests.append(g.digest(s, player))
        if int(time.time() - t0) // 40 >= shots:
            shots += 1
            s.save_png(os.path.join(g.wd, f"yt-play{tag}-{shots:02d}.png"))
        if g.new_bad():
            break
        time.sleep(4.0)
    changes = sum(1 for i in range(1, len(digests)) if digests[i] != digests[i - 1])
    levels = cs.wav_levels(wav, start=wav0)
    loud = sum(1 for v in levels if v > 40)
    picture = len(digests) >= seconds // 6 and changes >= (len(digests) - 1) * 0.9
    sound = len(levels) >= seconds // 5 - 2 and loud >= len(levels) * 0.8
    return {"picture": picture, "sound": sound, "changes": changes, "samples": len(digests), "loud": loud, "windows": len(levels),
            "consent": answered}


def main():
    import argparse
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--image", default="eurokernel.img")
    ap.add_argument("--pack", required=True)
    ap.add_argument("--nss", required=True)
    ap.add_argument("--workdir", required=True)
    ap.add_argument("--kvm", action="store_true")
    ap.add_argument("--smp", type=int, default=2)
    ap.add_argument("--mem", default="8192M")
    ap.add_argument("--pin", default="")
    ap.add_argument("--input", choices=["usb", "virtio"], default="usb")
    ap.add_argument("--nic", default="virtio-net-pci")
    ap.add_argument("--extra", default="")
    ap.add_argument("--sites", default="", help="comma list of site names (default: all of the matrix)")
    ap.add_argument("--hours", type=float, default=2.0)
    ap.add_argument("--video-seconds", type=int, default=120)
    ap.add_argument("--no-video", action="store_true")
    a = ap.parse_args()
    # The fields chrome-smoke's own argument parser would have set.
    a.only, a.rounds, a.soak_hours, a.starts = "", 1, a.hours, 0
    if cs.gs.OVMF is None:
        sys.exit("no OVMF firmware found")
    a.workdir = os.path.abspath(a.workdir)
    os.makedirs(a.workdir, exist_ok=True)
    sites = [s for s in cs.SITES if not a.sites or s["name"] in a.sites.split(",")]
    t0 = time.time()
    extra = {}

    def body(g):
        if not g.start_chrome("file:///tmp/input-test.html"):
            return
        g.close_infobars()
        g.move(g.w - 4, g.h - 4)
        g.calibrate_idle_icon()
        g.step("chrome's infobars are closed or absent", g.infobars == 0, f"{g.infobars} left")
        extra["memory_start"] = g.memory()
        end = time.time() + a.hours * 3600
        rnd, mem = 0, []
        while time.time() < end:
            rnd += 1
            tag = f"-r{rnd}" if rnd <= 3 else "-last"
            g.site_matrix(sites, tag=tag)
            if not a.no_video:
                y = youtube_play(g, a.video_seconds, rnd)
                ram = g.ramfiles()      # the census of the kernel, with the video still on the page
                if y:
                    g.step("site youtube-play", y["picture"] and y["sound"],
                           f"round {rnd}: {a.video_seconds} s of {cs.VIDEO_URL}: picture: {y['changes']} of {y['samples'] - 1} consecutive "
                           f"screendumps of the player differ; sound: {y['loud']} of {y['windows']} five-second windows above the noise floor; "
                           f"consent dialog: {'none shown' if y['consent'] is None else ('answered' if y['consent'] else 'NOT answered')}; RAM files {ram}")
                else:
                    g.step("site youtube-play", False, f"round {rnd}: the watch page did not load; RAM files {ram}")
            mem.append((int(time.time() - t0), g.memory()))
            print(f"[chrome-mem-soak] round {rnd} done after {int(time.time() - t0)} s: {mem[-1][1]}", flush=True)
            extra["soak_rounds"], extra["soak_memory"] = rnd, mem
            cs.write_report(a, a.workdir, g, "running", t0, extra)
        extra["memory_end"] = g.memory()

    g, verdict, rc = cs.one_boot(a, a.workdir, body)
    cs.write_report(a, a.workdir, g, verdict, t0, extra)
    print(f"[chrome-mem-soak] {verdict}: {extra.get('soak_rounds', 0)} rounds in {int(time.time() - t0)} s", flush=True)
    sys.exit(rc)


if __name__ == "__main__":
    main()
