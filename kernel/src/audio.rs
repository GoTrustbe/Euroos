//! Kernel side of **audio routing** (3F-6): a live [`euroaudio::Router`] — the
//! desktop audio server. Apps open per-app streams, each with its own volume +
//! mute, routed to an output device chosen by a default-device policy; the
//! router mixes exactly the streams on a device into one PCM buffer, which the
//! HDA driver plays. The routing/volume logic is host-tested in [`euroaudio`];
//! here we hold the live router, run the `[3f6]` self-test, and expose `audio`.

use alloc::string::String;
use alloc::vec::Vec;

use euroaudio::{Router, UNITY};
use spin::Mutex;

static ROUTER: Mutex<Option<Router>> = Mutex::new(None);

fn with_router<R>(f: impl FnOnce(&mut Router) -> R) -> R {
    let mut g = ROUTER.lock();
    let r = g.get_or_insert_with(|| {
        // Default device set = whatever the HDA driver found, else a virtual sink.
        let mut r = Router::new();
        if crate::hda::present() {
            r.add_device(1, "HD Audio");
        } else {
            r.add_device(1, "Virtual sink");
        }
        r.add_device(2, "Headphones");
        r
    });
    f(r)
}

/// Open an app playback stream (routed to the default device).
pub fn open_stream(app: &str) -> u32 {
    with_router(|r| r.open_stream(app))
}

/// Render the mix for the default device (what HDA plays).
pub fn render_default(frames: usize) -> Vec<i16> {
    with_router(|r| {
        let dev = r.default_device();
        r.render(dev, frames)
    })
}

/// `[3f6]` boot self-test — per-app streams, per-app routing to different
/// devices, per-stream + master volume/mute, and a default-device hotplug
/// re-route, all mixed through the host-tested router.
pub fn selftest() {
    let mut r = Router::new();
    r.add_device(1, "Speakers");
    r.add_device(2, "Headphones");

    let music = r.open_stream("euromusic");
    let call = r.open_stream("euromeet");
    r.submit(music, &[100, 100, 100, 100]);
    r.submit(call, &[40, 40, 40, 40]);

    // Both on the default device (1) → they mix.
    let mixed = r.render(1, 4) == alloc::vec![140, 140, 140, 140];

    // Route the call to headphones → device 1 now hears only music.
    r.route(call, 2);
    let split = r.render(1, 4) == alloc::vec![100; 4] && r.render(2, 4) == alloc::vec![40; 4];

    // Per-stream volume: halve music.
    r.set_stream_volume(music, UNITY / 2);
    let per_stream_vol = r.render(1, 4) == alloc::vec![50; 4];

    // Master mute silences everything.
    r.set_master_muted(true);
    let master_mute = r.render(1, 4) == alloc::vec![0; 4] && r.render(2, 4) == alloc::vec![0; 4];
    r.set_master_muted(false);

    // Unplug the default device → default + streams fall back to device 2.
    r.remove_device(1);
    let reroute = r.default_device() == 2;

    *ROUTER.lock() = Some(r);

    let ok = mixed && split && per_stream_vol && master_mute && reroute;
    crate::serial_println!(
        "[3f6] audio routing (euroaudio::Router): per-app-mix={mixed}, per-app-route-to-device={split}, per-stream-volume={per_stream_vol}, master-mute={master_mute}, default-device-hotplug-reroute={reroute} → {}",
        if ok { "OK (mixer + per-app routing + device policy; HDA plays render()) ✓" } else { "FAILED ✗" }
    );
}

/// `audio` shell command: show devices, the default, master state, and the
/// per-app stream table (the mixer view).
pub fn shell() -> Vec<String> {
    with_router(|r| {
        let mut out = alloc::vec![String::from("EuroAudio — routing & mixer (3F-6)")];
        let def = r.default_device();
        out.push(String::from("  devices:"));
        for d in r.devices() {
            let mark = if d.id == def { " (default)" } else { "" };
            out.push(alloc::format!("    [{}] {}{}{}", d.id, d.name, if d.present { "" } else { " (absent)" }, mark));
        }
        let streams = r.stream_summary();
        if streams.is_empty() {
            out.push(String::from("  no active streams"));
        } else {
            out.push(String::from("  streams (app · vol · mute · device):"));
            for (app, id, vol, muted, dev) in streams {
                out.push(alloc::format!(
                    "    #{id:<3} {app:<12} {:>3}% {} → dev {dev}",
                    (vol as u32 * 100 / UNITY as u32),
                    if muted { "MUTED" } else { "     " }
                ));
            }
        }
        out.push(String::from("  the HDA driver plays render(default_device) each period"));
        out
    })
}
