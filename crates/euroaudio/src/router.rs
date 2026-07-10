//! **Audio routing & desktop integration** (3F-6) — the server layer the mixer
//! was missing: a table of **per-app streams** (each with its own volume + mute),
//! a set of **output devices** with a **default-device policy**, per-stream
//! **routing** to a device, and a **master** volume/mute. `render(device)` mixes
//! exactly the streams routed to that device into one PCM buffer via the
//! host-tested [`crate::mix`].
//!
//! Pure `no_std` bookkeeping + arithmetic, so per-app routing and the volume law
//! are host-tested independently of any sound card (the HDA driver just consumes
//! `render()`'s output).

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::{mix, Volume, UNITY};

/// An output device (speakers, headphones, HDMI, a virtual sink…).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: u32,
    pub name: String,
    /// True while the device is physically present (hotplug).
    pub present: bool,
}

/// One application playback stream.
#[derive(Debug, Clone)]
pub struct Stream {
    pub id: u32,
    pub app: String,
    pub volume: Volume,
    pub muted: bool,
    /// The device this stream is routed to (its id).
    pub device: u32,
    /// The pending PCM for the next `render` (i16 interleaved).
    pub pcm: Vec<i16>,
}

/// The audio router / server.
pub struct Router {
    devices: Vec<Device>,
    streams: Vec<Stream>,
    default_device: u32,
    master_volume: Volume,
    master_muted: bool,
    next_stream: u32,
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

impl Router {
    pub fn new() -> Self {
        Self {
            devices: Vec::new(),
            streams: Vec::new(),
            default_device: 0,
            master_volume: UNITY,
            master_muted: false,
            next_stream: 1,
        }
    }

    // ── devices ───────────────────────────────────────────────────────────
    /// Register/hotplug an output device. The first device added becomes the
    /// default.
    pub fn add_device(&mut self, id: u32, name: &str) {
        if self.devices.iter().any(|d| d.id == id) {
            return;
        }
        let first = self.devices.is_empty();
        self.devices.push(Device { id, name: name.to_string(), present: true });
        if first {
            self.default_device = id;
        }
    }

    /// Remove a device (unplug). If it was the default, the default falls back to
    /// the next present device, and its streams are re-routed there.
    pub fn remove_device(&mut self, id: u32) {
        self.devices.retain(|d| d.id != id);
        if self.default_device == id {
            self.default_device = self.devices.iter().find(|d| d.present).map(|d| d.id).unwrap_or(0);
        }
        // Re-route orphaned streams to the (new) default.
        let def = self.default_device;
        for s in self.streams.iter_mut() {
            if s.device == id {
                s.device = def;
            }
        }
    }

    /// Set the default output device (the desktop's default-device policy).
    /// Returns false if the device is unknown/absent.
    pub fn set_default_device(&mut self, id: u32) -> bool {
        if self.devices.iter().any(|d| d.id == id && d.present) {
            self.default_device = id;
            true
        } else {
            false
        }
    }

    pub fn default_device(&self) -> u32 {
        self.default_device
    }

    pub fn devices(&self) -> &[Device] {
        &self.devices
    }

    // ── streams ───────────────────────────────────────────────────────────
    /// Open an app playback stream, routed to the current default device.
    pub fn open_stream(&mut self, app: &str) -> u32 {
        let id = self.next_stream;
        self.next_stream += 1;
        self.streams.push(Stream {
            id,
            app: app.to_string(),
            volume: UNITY,
            muted: false,
            device: self.default_device,
            pcm: Vec::new(),
        });
        id
    }

    pub fn close_stream(&mut self, id: u32) {
        self.streams.retain(|s| s.id != id);
    }

    /// Route a stream to a specific device (per-app routing).
    pub fn route(&mut self, stream: u32, device: u32) -> bool {
        let known = self.devices.iter().any(|d| d.id == device);
        if !known {
            return false;
        }
        if let Some(s) = self.streams.iter_mut().find(|s| s.id == stream) {
            s.device = device;
            true
        } else {
            false
        }
    }

    pub fn set_stream_volume(&mut self, stream: u32, vol: Volume) {
        if let Some(s) = self.streams.iter_mut().find(|s| s.id == stream) {
            s.volume = vol;
        }
    }

    pub fn set_stream_muted(&mut self, stream: u32, muted: bool) {
        if let Some(s) = self.streams.iter_mut().find(|s| s.id == stream) {
            s.muted = muted;
        }
    }

    /// Queue PCM for a stream to be mixed on the next `render`.
    pub fn submit(&mut self, stream: u32, pcm: &[i16]) {
        if let Some(s) = self.streams.iter_mut().find(|s| s.id == stream) {
            s.pcm = pcm.to_vec();
        }
    }

    // ── master ────────────────────────────────────────────────────────────
    pub fn set_master_volume(&mut self, vol: Volume) {
        self.master_volume = vol;
    }
    pub fn set_master_muted(&mut self, muted: bool) {
        self.master_muted = muted;
    }

    /// Mix exactly the (unmuted) streams routed to `device` into `frames`-long
    /// PCM, applying each stream's volume, then the master volume. A muted master
    /// yields silence. This is what the HDA driver plays.
    pub fn render(&self, device: u32, frames: usize) -> Vec<i16> {
        let mut out = vec![0i16; frames];
        if self.master_muted {
            return out;
        }
        // Effective per-stream volume = stream_vol * master_vol (Q8·Q8 → Q8).
        let inputs: Vec<(Vec<i16>, Volume)> = self
            .streams
            .iter()
            .filter(|s| s.device == device && !s.muted && !s.pcm.is_empty())
            .map(|s| {
                let mut buf = vec![0i16; frames];
                let n = s.pcm.len().min(frames);
                buf[..n].copy_from_slice(&s.pcm[..n]);
                let v = ((s.volume as u32 * self.master_volume as u32) >> 8) as Volume;
                (buf, v)
            })
            .collect();
        let refs: Vec<(&[i16], Volume)> = inputs.iter().map(|(b, v)| (b.as_slice(), *v)).collect();
        mix(&refs, &mut out);
        out
    }

    /// A snapshot for the mixer UI: `(app, stream_id, volume, muted, device)`.
    pub fn stream_summary(&self) -> Vec<(String, u32, Volume, bool, u32)> {
        self.streams.iter().map(|s| (s.app.clone(), s.id, s.volume, s.muted, s.device)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router() -> Router {
        let mut r = Router::new();
        r.add_device(1, "Speakers");
        r.add_device(2, "Headphones");
        r
    }

    #[test]
    fn first_device_becomes_default_and_streams_follow_it() {
        let mut r = router();
        assert_eq!(r.default_device(), 1);
        let s = r.open_stream("music");
        // The new stream is routed to the default device.
        assert_eq!(r.stream_summary()[0].4, 1);
        r.set_default_device(2);
        // Existing stream stays where it was; a NEW stream follows the new default.
        let s2 = r.open_stream("browser");
        assert_eq!(r.stream_summary().iter().find(|x| x.1 == s).unwrap().4, 1);
        assert_eq!(r.stream_summary().iter().find(|x| x.1 == s2).unwrap().4, 2);
    }

    #[test]
    fn render_mixes_only_streams_on_that_device() {
        let mut r = router();
        let a = r.open_stream("a");
        let b = r.open_stream("b");
        r.route(b, 2); // move b to headphones
        r.submit(a, &[100, 100, 100, 100]);
        r.submit(b, &[50, 50, 50, 50]);
        // Device 1 hears only stream a.
        assert_eq!(r.render(1, 4), vec![100, 100, 100, 100]);
        // Device 2 hears only stream b.
        assert_eq!(r.render(2, 4), vec![50, 50, 50, 50]);
    }

    #[test]
    fn per_stream_volume_and_mute() {
        let mut r = router();
        let a = r.open_stream("a");
        r.submit(a, &[200, 200]);
        r.set_stream_volume(a, UNITY / 2); // half
        assert_eq!(r.render(1, 2), vec![100, 100]);
        r.set_stream_muted(a, true);
        assert_eq!(r.render(1, 2), vec![0, 0]);
    }

    #[test]
    fn master_volume_and_mute_apply_after_mixing() {
        let mut r = router();
        let a = r.open_stream("a");
        r.submit(a, &[200, 200]);
        r.set_master_volume(UNITY / 4); // quarter
        assert_eq!(r.render(1, 2), vec![50, 50]);
        r.set_master_muted(true);
        assert_eq!(r.render(1, 2), vec![0, 0]);
    }

    #[test]
    fn unplugging_the_default_reroutes_streams_and_default() {
        let mut r = router();
        let a = r.open_stream("a"); // on device 1 (default)
        r.submit(a, &[10, 10]);
        r.remove_device(1);
        // Default fell back to device 2, and the stream followed.
        assert_eq!(r.default_device(), 2);
        assert_eq!(r.stream_summary()[0].4, 2);
        assert_eq!(r.render(2, 2), vec![10, 10]);
    }

    #[test]
    fn two_streams_on_one_device_mix() {
        let mut r = router();
        let a = r.open_stream("a");
        let b = r.open_stream("b");
        r.submit(a, &[100, 100]);
        r.submit(b, &[50, 50]);
        assert_eq!(r.render(1, 2), vec![150, 150]);
    }
}
