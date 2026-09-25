//! ALSA-compatible PCM playback device over the HD-Audio driver (workplace
//! sprint W12). Chromium's audio output on Linux dlopens libasound and opens
//! `/dev/snd/pcmC0D0p` through the `hw` plugin, which speaks a small set of
//! ioctls: version and info, hardware and software parameters, prepare/start/
//! drop, interleaved writes, status and pointer sync. This module answers them
//! over the one cyclic DMA buffer the HDA driver already runs (48 kHz, 16-bit,
//! stereo), so any ALSA client plays without a libasound of its own.
//!
//! Model: the DMA loops over a ring of `ring_frames` stereo frames; the link
//! position (LPIB) gives the hardware read cursor, tracked into a monotonic
//! `hw_ptr` in frames. The client writes ahead of it (`appl_ptr`); what the
//! hardware has consumed is zeroed behind the cursor so silence, not a stale
//! period, plays when the client falls behind. Access is RW_INTERLEAVED only
//! (no mmap of the buffer: libasound falls back to SYNC_PTR when the status
//! pages cannot be mapped, and chromium writes with snd_pcm_writei).
use spin::Mutex;

// EXACTLY the kernel's SNDRV_PCM_STATE_* values. They were each one too high
// (PREPARED was 3), so chrome read our PREPARED as RUNNING and our RUNNING as
// XRUN: after PREPARE it saw a running-but-unfed stream, recovered with
// XRUN+PREPARE, and looped 77 times without ever writing a frame (runs 86-89).
const STATE_OPEN: u32 = 0;
const STATE_SETUP: u32 = 1;
const STATE_PREPARED: u32 = 2;
const STATE_RUNNING: u32 = 3;
const STATE_XRUN: u32 = 4;
const STATE_PAUSED: u32 = 6;

struct Pcm {
    state: u32,
    ring_frames: u64,
    period_size: u64,
    buffer_size: u64,
    appl_ptr: u64,
    hw_ptr: u64,
    last_lpib: u32,
    avail_min: u64,
    start_threshold: u64,
    zeroed_upto: u64,
    opens: u32,
}

static PCM: Mutex<Pcm> = Mutex::new(Pcm {
    state: STATE_OPEN, ring_frames: 0, period_size: 1024, buffer_size: 0,
    appl_ptr: 0, hw_ptr: 0, last_lpib: 0, avail_min: 1, start_threshold: 1, zeroed_upto: 0, opens: 0,
});
static LINES: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
fn log(what: &str) {
    if LINES.fetch_add(1, core::sync::atomic::Ordering::Relaxed) < 120 {
        crate::serial_println!("[alsa] @{} {what}", crate::interrupts::ticks());
    }
}

const FRAME_BYTES: u64 = 4; // 16-bit stereo

/// The ring's physical address and size, for the data mmap.
pub fn ring_phys() -> Option<(u64, usize)> {
    crate::hda::pcm_ring()
}

/// The device exists when the HDA stream runs.
pub fn present() -> bool {
    crate::hda::pcm_ring().is_some()
}

/// open("/dev/snd/pcmC0D0p"): silence the ring, reset the pointers.
pub fn pcm_open() {
    let (_, bytes) = crate::hda::pcm_ring().unwrap_or((0, 0));
    let mut p = PCM.lock();
    p.ring_frames = bytes as u64 / FRAME_BYTES;
    if p.buffer_size == 0 || p.buffer_size > p.ring_frames {
        p.buffer_size = p.ring_frames;
    }
    p.state = STATE_OPEN;
    p.appl_ptr = 0;
    p.hw_ptr = 0;
    p.last_lpib = crate::hda::pcm_lpib();
    p.zeroed_upto = 0;
    p.opens += 1;
    crate::hda::pcm_zero(0, bytes);
    log(&alloc::format!("open #{}: ring {} frames ({} ms)", p.opens, p.ring_frames, p.ring_frames * 1000 / 48000));
}

pub fn pcm_close() {
    let mut p = PCM.lock();
    p.state = STATE_OPEN;
    let bytes = crate::hda::pcm_ring().map(|(_, b)| b).unwrap_or(0);
    crate::hda::pcm_zero(0, bytes);
    log("close");
}

/// Advance hw_ptr to the DMA's real position, so the client paces its writes to
/// real time (avail = space ahead of the hardware). The client's samples in the
/// ring ahead of hw_ptr are what the DMA plays next; we do NOT zero or clamp
/// anything here. An earlier version clamped hw_ptr to appl_ptr and zeroed the
/// consumed span, but the real DMA reads at LPIB, not at the clamped pointer, so
/// the zeroing erased the samples chrome had just written and the output went
/// silent (run 90: the tone's clock reached 7.77 s but the WAV was silent). The
/// ring is zeroed once at open and again at drop, so no stale buffer loops; while
/// the stream runs the client keeps it full.
fn sync(p: &mut Pcm) {
    if p.ring_frames == 0 {
        return;
    }
    let lpib = crate::hda::pcm_lpib();
    let ring_bytes = p.ring_frames * FRAME_BYTES;
    let delta = (lpib as u64 + ring_bytes - p.last_lpib as u64) % ring_bytes;
    p.last_lpib = lpib;
    if p.state == STATE_RUNNING {
        p.hw_ptr += delta / FRAME_BYTES;
    }
}

fn avail(p: &Pcm) -> u64 {
    p.buffer_size.saturating_sub(p.appl_ptr.saturating_sub(p.hw_ptr))
}

fn put_interval(buf: &mut [u8], idx: usize, min: u32, max: u32) {
    let o = 260 + idx * 12;
    buf[o..o + 4].copy_from_slice(&min.to_le_bytes());
    buf[o + 4..o + 8].copy_from_slice(&max.to_le_bytes());
    let flags: u32 = 1 << 2; // integer
    buf[o + 8..o + 12].copy_from_slice(&flags.to_le_bytes());
}
fn get_interval(buf: &[u8], idx: usize) -> (u32, u32) {
    let o = 260 + idx * 12;
    (u32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]),
     u32::from_le_bytes([buf[o + 4], buf[o + 5], buf[o + 6], buf[o + 7]]))
}
fn put_mask(buf: &mut [u8], idx: usize, bit: u32) {
    let o = 4 + idx * 32;
    for b in &mut buf[o..o + 32] { *b = 0; }
    let w = (bit / 32) as usize;
    let v = 1u32 << (bit % 32);
    buf[o + w * 4..o + w * 4 + 4].copy_from_slice(&v.to_le_bytes());
}
fn mask_allows(buf: &[u8], idx: usize, bit: u32) -> bool {
    let o = 4 + idx * 32 + (bit / 32) as usize * 4;
    let w = u32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]);
    w & (1 << (bit % 32)) != 0
}

/// Pick a value inside the client's interval, preferring `want`, then clamped
/// to [lo, hi]; None when the intervals cannot meet.
fn choose(req: (u32, u32), want: u32, lo: u32, hi: u32) -> Option<u32> {
    let (rmin, rmax) = req;
    let lo = lo.max(rmin);
    let hi = hi.min(rmax);
    if lo > hi { return None; }
    Some(want.clamp(lo, hi))
}

/// HW_REFINE / HW_PARAMS: reduce the client's parameter space to what the ring
/// does: RW_INTERLEAVED, S16_LE, standard subformat, 2 channels, 48 kHz.
fn hw_params(arg: u64, commit: bool) -> u64 {
    let Some(mut buf) = crate::ring3::copy_from_user(arg, 608) else { return EFAULT };
    // What the client asks, every call (bounded by the line budget): run 78's
    // set_params failed "Rate 48000Hz not available" with no refusal logged here.
    {
        let w = |o: usize| u32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]);
        log(&alloc::format!(
            "{} rmask={:#x} access={:#x} format={:#x}/{:#x} ch=[{},{}] rate=[{},{}] period=[{},{}] buffer=[{},{}] info={:#x}",
            if commit { "hw_params" } else { "hw_refine" }, w(512), w(4), w(36), w(40),
            get_interval(&buf, 2).0, get_interval(&buf, 2).1, get_interval(&buf, 3).0, get_interval(&buf, 3).1,
            get_interval(&buf, 5).0, get_interval(&buf, 5).1, get_interval(&buf, 9).0, get_interval(&buf, 9).1, w(520)));
    }
    // The first 32-bit word of each mask (params fit in it), snapshotted so cmask
    // can name only what this refine actually narrowed. A mask that comes back
    // different from the client's makes libasound re-refine; a mask reported as
    // "changed" when it did not change makes it re-refine forever (run 80: the
    // same refine 100 times, then set_params failed). So cmask must be exact.
    let mask_word = |b: &[u8], idx: usize| u32::from_le_bytes([b[4 + idx * 32], b[5 + idx * 32], b[6 + idx * 32], b[7 + idx * 32]]);
    let in_masks = [mask_word(&buf, 0), mask_word(&buf, 1), mask_word(&buf, 2)];
    let in_ivals: [(u32, u32); 12] = core::array::from_fn(|i| get_interval(&buf, i));
    // Masks: ACCESS bit 0 (MMAP_INTERLEAVED) and/or bit 3 (RW_INTERLEAVED), FORMAT
    // bit 2 (S16_LE), SUBFORMAT bit 0. Keep BOTH access kinds the client offered
    // and we support: chrome's set_params picks RW_INTERLEAVED for writei, the plug
    // layer drives its slave via MMAP, and either must stay available or set_params
    // fails "Rate not available" three steps later (run 80: access forced to
    // MMAP-only, so the RW set_access left an empty space that the rate refine saw).
    let mmap_ok = mask_allows(&buf, 0, 0);
    let rw_ok = mask_allows(&buf, 0, 3);
    if !(mmap_ok || rw_ok) || !mask_allows(&buf, 1, 2) || !mask_allows(&buf, 2, 0) {
        log("hw_params: access/format not offered by the client");
        return EINVAL;
    }
    // ACCESS: RW_INTERLEAVED only. chrome writes with snd_pcm_writei, and
    // advertising MMAP sent libasound down its mmap path, where snd_pcm_avail_update
    // calls the XRUN ioctl (0x4148) we do not serve -> "Failed querying available
    // frames" and no audio (run 85). RW keeps it on writei + SYNC_PTR/STATUS, which
    // the kernel serves. (mmap_ok is still required above so a client that offers
    // only MMAP is not rejected outright, but we do not select it.)
    let _ = mmap_ok;
    {
        for b in &mut buf[4..4 + 32] { *b = 0; }
        buf[4..8].copy_from_slice(&(1u32 << 3).to_le_bytes());
    }
    put_mask(&mut buf, 1, 2);
    put_mask(&mut buf, 2, 0);
    let ring = PCM.lock().ring_frames.max(2048) as u32;
    // Intervals, index = param - 8: SAMPLE_BITS 0, FRAME_BITS 1, CHANNELS 2, RATE 3,
    // PERIOD_TIME 4, PERIOD_SIZE 5, PERIOD_BYTES 6, PERIODS 7, BUFFER_TIME 8,
    // BUFFER_SIZE 9, BUFFER_BYTES 10, TICK_TIME 11.
    let fixed = [(0usize, 16u32), (1, 32), (2, 2), (3, 48000)];
    for &(i, v) in &fixed {
        let (mn, mx) = get_interval(&buf, i);
        if mn > v || mx < v {
            log(&alloc::format!("hw_params: interval {i} [{mn},{mx}] excludes {v}"));
            return EINVAL;
        }
        put_interval(&mut buf, i, v, v);
    }
    let Some(period) = choose(get_interval(&buf, 5), 1024, 240, ring / 2) else { log("hw_params: period interval cannot meet the ring"); return EINVAL };
    let Some(buffer) = choose(get_interval(&buf, 9), ring, period * 2, ring) else { log("hw_params: buffer interval cannot meet the ring"); return EINVAL };
    let buffer = buffer - buffer % period;
    let periods = buffer / period;
    put_interval(&mut buf, 5, period, period);
    put_interval(&mut buf, 6, period * 4, period * 4);
    put_interval(&mut buf, 4, period * 1000000 / 48000, period * 1000000 / 48000);
    put_interval(&mut buf, 7, periods, periods);
    put_interval(&mut buf, 9, buffer, buffer);
    put_interval(&mut buf, 10, buffer * 4, buffer * 4);
    put_interval(&mut buf, 8, buffer * 1000000 / 48000, buffer * 1000000 / 48000);
    put_interval(&mut buf, 11, 0, 0);
    // cmask: exactly the params this refine narrowed (mask params 0..2, interval
    // params 8..19 = interval index + 8). rmask is left as the client set it.
    let mut cmask = 0u32;
    for idx in 0..3 {
        if mask_word(&buf, idx) != in_masks[idx] { cmask |= 1 << idx; }
    }
    for i in 0..12 {
        if get_interval(&buf, i) != in_ivals[i] { cmask |= 1 << (i + 8); }
    }
    buf[516..520].copy_from_slice(&cmask.to_le_bytes());
    // info: INTERLEAVED | BLOCK_TRANSFER (0x10000). NOT MMAP: see the access note.
    buf[520..524].copy_from_slice(&(0x0000_0100u32 | 0x0001_0000).to_le_bytes());
    buf[524..528].copy_from_slice(&16u32.to_le_bytes()); // msbits
    buf[528..532].copy_from_slice(&48000u32.to_le_bytes()); // rate_num
    buf[532..536].copy_from_slice(&1u32.to_le_bytes()); // rate_den
    buf[536..544].copy_from_slice(&0u64.to_le_bytes()); // fifo_size
    {
        let mw = |b: &[u8], idx: usize| u32::from_le_bytes([b[4 + idx * 32], b[5 + idx * 32], b[6 + idx * 32], b[7 + idx * 32]]);
        log(&alloc::format!(
            "-> access={:#x} fmt={:#x} rate=[{},{}] period=[{},{}] buffer=[{},{}] cmask={:#x} info={:#x}",
            mw(&buf, 0), mw(&buf, 1), get_interval(&buf, 3).0, get_interval(&buf, 3).1,
            get_interval(&buf, 5).0, get_interval(&buf, 5).1, get_interval(&buf, 9).0, get_interval(&buf, 9).1,
            cmask, u32::from_le_bytes([buf[520], buf[521], buf[522], buf[523]])));
    }
    if !crate::ring3::copy_to_user(arg, &buf) { return EFAULT; }
    if commit {
        let mut p = PCM.lock();
        p.period_size = period as u64;
        p.buffer_size = buffer as u64;
        p.state = STATE_SETUP;
        log(&alloc::format!("hw_params: period {period} frames, buffer {buffer} frames"));
    }
    0
}

fn sw_params(arg: u64) -> u64 {
    let Some(mut buf) = crate::ring3::copy_from_user(arg, 136) else { return EFAULT };
    let rd = |o: usize| u64::from_le_bytes([buf[o], buf[o+1], buf[o+2], buf[o+3], buf[o+4], buf[o+5], buf[o+6], buf[o+7]]);
    let avail_min = rd(16);
    let start = rd(32);
    let mut p = PCM.lock();
    p.avail_min = avail_min.max(1);
    p.start_threshold = if start == 0 { 1 } else { start };
    // boundary (off 64): a large multiple of the buffer size, as Linux does.
    let mut boundary = p.buffer_size.max(1);
    while boundary < (1u64 << 40) { boundary *= 2; }
    buf[64..72].copy_from_slice(&boundary.to_le_bytes());
    drop(p);
    if !crate::ring3::copy_to_user(arg, &buf) { return EFAULT; }
    0
}

fn status(arg: u64, ext: bool) -> u64 {
    let mut buf = [0u8; 152];
    let mut p = PCM.lock();
    sync(&mut p);
    let delay = p.appl_ptr.saturating_sub(p.hw_ptr);
    buf[0..4].copy_from_slice(&p.state.to_le_bytes());
    let t = crate::interrupts::ticks();
    let secs = t / 100; let nsec = (t % 100) * 10_000_000;
    for o in [8usize, 24] {
        buf[o..o + 8].copy_from_slice(&secs.to_le_bytes());
        buf[o + 8..o + 16].copy_from_slice(&nsec.to_le_bytes());
    }
    buf[40..48].copy_from_slice(&p.appl_ptr.to_le_bytes());
    buf[48..56].copy_from_slice(&p.hw_ptr.to_le_bytes());
    buf[56..64].copy_from_slice(&(delay as i64).to_le_bytes());
    buf[64..72].copy_from_slice(&avail(&p).to_le_bytes());
    buf[72..80].copy_from_slice(&p.buffer_size.to_le_bytes());
    let _ = ext;
    drop(p);
    if !crate::ring3::copy_to_user(arg, &buf) { return EFAULT; }
    0
}

fn sync_ptr(arg: u64) -> u64 {
    let Some(mut buf) = crate::ring3::copy_from_user(arg, 136) else { return EFAULT };
    let flags = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let mut p = PCM.lock();
    if flags & 2 == 0 { // SNDRV_PCM_SYNC_PTR_APPL clear: the client's appl_ptr is authoritative
        let a = u64::from_le_bytes([buf[72], buf[73], buf[74], buf[75], buf[76], buf[77], buf[78], buf[79]]);
        p.appl_ptr = a;
    }
    if flags & 4 == 0 { // SNDRV_PCM_SYNC_PTR_AVAIL_MIN clear
        let m = u64::from_le_bytes([buf[80], buf[81], buf[82], buf[83], buf[84], buf[85], buf[86], buf[87]]);
        p.avail_min = m.max(1);
    }
    sync(&mut p);
    if p.state == STATE_PREPARED && p.appl_ptr >= p.start_threshold {
        p.state = STATE_RUNNING;
    }
    buf[8..12].copy_from_slice(&p.state.to_le_bytes());
    buf[16..24].copy_from_slice(&p.hw_ptr.to_le_bytes());
    let t = crate::interrupts::ticks();
    buf[24..32].copy_from_slice(&(t / 100).to_le_bytes());
    buf[32..40].copy_from_slice(&((t % 100) * 10_000_000).to_le_bytes());
    buf[72..80].copy_from_slice(&p.appl_ptr.to_le_bytes());
    buf[80..88].copy_from_slice(&p.avail_min.to_le_bytes());
    drop(p);
    if !crate::ring3::copy_to_user(arg, &buf) { return EFAULT; }
    0
}

fn writei(arg: u64) -> u64 {
    let Some(x) = crate::ring3::copy_from_user(arg, 24) else { return EFAULT };
    let src = u64::from_le_bytes([x[8], x[9], x[10], x[11], x[12], x[13], x[14], x[15]]);
    let frames = u64::from_le_bytes([x[16], x[17], x[18], x[19], x[20], x[21], x[22], x[23]]);
    let mut p = PCM.lock();
    if p.state == STATE_OPEN || p.state == STATE_SETUP {
        log(&alloc::format!("writei EBADFD (state {} appl {})", p.state, p.appl_ptr));
        return EBADFD;
    }
    sync(&mut p);
    if p.state == STATE_XRUN {
        log("writei EPIPE (xrun)");
        return EPIPE; // the client recovers with PREPARE
    }
    let room = avail(&p);
    let n = frames.min(room);
    if n == 0 {
        log(&alloc::format!("writei EAGAIN (avail 0, appl {} hw {})", p.appl_ptr, p.hw_ptr));
        return EAGAIN;
    }
    let Some(data) = crate::ring3::copy_from_user(src, (n * FRAME_BYTES) as usize) else { return EFAULT };
    let start = ((p.appl_ptr % p.ring_frames) * FRAME_BYTES) as usize;
    crate::hda::pcm_write(start, &data);
    p.appl_ptr += n;
    if p.state == STATE_PREPARED && p.appl_ptr >= p.start_threshold {
        p.state = STATE_RUNNING;
        log(&alloc::format!("running (start threshold {} frames)", p.start_threshold));
    }
    static WRITES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
    let w = WRITES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    if w < 3 || w % 500 == 0 {
        log(&alloc::format!("writei #{} {n} of {frames} frames; appl {} hw {} state {}", w + 1, p.appl_ptr, p.hw_ptr, p.state));
    }
    drop(p);
    let res = (n as i64).to_le_bytes();
    if !crate::ring3::copy_to_user(arg, &res) { return EFAULT; }
    0
}

const EFAULT: u64 = (-14i64) as u64;
const EINVAL: u64 = (-22i64) as u64;
const EAGAIN: u64 = (-11i64) as u64;
const EPIPE: u64 = (-32i64) as u64;
const EBADFD: u64 = (-77i64) as u64;
const ENOTTY: u64 = (-25i64) as u64;

/// ioctl on /dev/snd/pcmC0D0p.
pub fn pcm_ioctl(cmd: u64, arg: u64) -> u64 {
    match cmd as u32 {
        0x8004_4100 => { if crate::ring3::write_user::<i32>(arg, 0x20012) { 0 } else { EFAULT } } // PVERSION 2.0.18
        0x4004_4104 | 0x4004_4103 | 0x4004_4102 => 0, // USER_PVERSION, TTSTAMP, TSTAMP
        0x4112 => { let mut p = PCM.lock(); p.state = STATE_OPEN; 0 } // HW_FREE
        0x8120_4101 => pcm_info(arg),
        0xc260_4110 => hw_params(arg, false),
        0xc260_4111 => hw_params(arg, true),
        0xc088_4113 => sw_params(arg),
        0x8098_4120 => status(arg, false),
        0xc098_4124 => status(arg, true),
        0x8008_4121 => { // DELAY
            let mut p = PCM.lock(); sync(&mut p);
            let d = p.appl_ptr.saturating_sub(p.hw_ptr) as i64;
            drop(p);
            if crate::ring3::write_user::<i64>(arg, d) { 0 } else { EFAULT }
        }
        0x4122 => { let mut p = PCM.lock(); sync(&mut p); 0 } // HWSYNC
        0xc088_4123 => sync_ptr(arg),
        0x4140 | 0x4141 => { // PREPARE, RESET
            let mut p = PCM.lock();
            let bytes = crate::hda::pcm_ring().map(|(_, b)| b).unwrap_or(0);
            crate::hda::pcm_zero(0, bytes);
            p.last_lpib = crate::hda::pcm_lpib();
            p.appl_ptr = 0; p.hw_ptr = 0; p.zeroed_upto = 0;
            p.state = STATE_PREPARED;
            log("prepared");
            0
        }
        0x4142 => { let mut p = PCM.lock(); if p.state == STATE_PREPARED { p.state = STATE_RUNNING; log("start"); 0 } else { EBADFD } }
        0x4143 | 0x4144 => { // DROP, DRAIN
            let mut p = PCM.lock(); p.state = STATE_SETUP;
            let bytes = crate::hda::pcm_ring().map(|(_, b)| b).unwrap_or(0);
            crate::hda::pcm_zero(0, bytes);
            log("drop/drain");
            0
        }
        0x4004_4145 => { // PAUSE(int)
            let on = crate::ring3::read_user::<i32>(arg).unwrap_or(0);
            let mut p = PCM.lock();
            p.state = if on != 0 { STATE_PAUSED } else { STATE_RUNNING };
            0
        }
        0x4018_4150 => writei(arg),
        0x8018_4132 => { // CHANNEL_INFO: channel at arg[0]; interleaved 16-bit stereo
            let ch = crate::ring3::read_user::<u32>(arg).unwrap_or(0);
            let mut b = [0u8; 24];
            b[0..4].copy_from_slice(&ch.to_le_bytes());
            b[16..20].copy_from_slice(&(ch * 16).to_le_bytes());
            b[20..24].copy_from_slice(&32u32.to_le_bytes());
            if crate::ring3::copy_to_user(arg, &b) { 0 } else { EFAULT }
        }
        0x4148 => 0, // XRUN: acknowledge; the RW path recovers by writing again
        0x4149 | 0x4146 => 0, // FORWARD, REWIND: no partial rewind, accept as no-op
        other => { log(&alloc::format!("pcm ioctl {other:#x} unsupported")); ENOTTY }
    }
}

fn pcm_info(arg: u64) -> u64 {
    let mut b = [0u8; 288];
    b[12..16].copy_from_slice(&0u32.to_le_bytes()); // card 0
    b[16..23].copy_from_slice(b"EuroHDA");
    b[80..95].copy_from_slice(b"EuroOS HD-Audio");
    b[160..172].copy_from_slice(b"subdevice #0");
    b[200..204].copy_from_slice(&1u32.to_le_bytes()); // subdevices_count
    b[204..208].copy_from_slice(&1u32.to_le_bytes()); // subdevices_avail
    if crate::ring3::copy_to_user(arg, &b) { 0 } else { EFAULT }
}

/// ioctl on /dev/snd/controlC0: enough for libasound to find card 0 with one
/// playback device and no mixer elements.
pub fn ctl_ioctl(cmd: u64, arg: u64) -> u64 {
    match cmd as u32 {
        0x8004_5500 => { if crate::ring3::write_user::<i32>(arg, 0x20009) { 0 } else { EFAULT } }
        0x8178_5501 => {
            let mut b = [0u8; 376];
            b[8..15].copy_from_slice(b"EuroHDA");
            b[24..31].copy_from_slice(b"eurohda"); // driver
            b[40..55].copy_from_slice(b"EuroOS HD-Audio");
            b[72..87].copy_from_slice(b"EuroOS HD-Audio");
            b[168..174].copy_from_slice(b"EuroOS");
            if crate::ring3::copy_to_user(arg, &b) { 0 } else { EFAULT }
        }
        0x8004_5530 => { // PCM_NEXT_DEVICE: -1 -> 0, else -1
            let cur = crate::ring3::read_user::<i32>(arg).unwrap_or(-1);
            let next: i32 = if cur < 0 { 0 } else { -1 };
            if crate::ring3::write_user::<i32>(arg, next) { 0 } else { EFAULT }
        }
        0xc120_5531 => pcm_info(arg),
        0x4004_5532 => 0, // PCM_PREFER_SUBDEVICE
        0xc050_5510 => { // ELEM_LIST: no elements
            let Some(mut b) = crate::ring3::copy_from_user(arg, 80) else { return EFAULT };
            b[8..12].copy_from_slice(&0u32.to_le_bytes()); // used
            b[12..16].copy_from_slice(&0u32.to_le_bytes()); // count
            if crate::ring3::copy_to_user(arg, &b) { 0 } else { EFAULT }
        }
        other => { log(&alloc::format!("ctl ioctl {other:#x} unsupported")); ENOTTY }
    }
}

/// poll(): the playback device is writable when at least avail_min frames are free.
pub fn pcm_writable() -> bool {
    let mut p = PCM.lock();
    sync(&mut p);
    p.state == STATE_OPEN || p.state == STATE_SETUP || avail(&p) >= p.avail_min || p.state == STATE_XRUN
}

/// A minimal alsa.conf for libasound (see the registration in the chrome setup).
pub const ALSA_CONF: &[u8] = b"defaults.pcm.card 0
defaults.pcm.device 0
defaults.pcm.subdevice -1
defaults.ctl.card 0
pcm.hw {
	@args [ CARD DEV SUBDEV ]
	@args.CARD { type string default 0 }
	@args.DEV { type integer default 0 }
	@args.SUBDEV { type integer default -1 }
	type hw
	card $CARD
	device $DEV
	subdevice $SUBDEV
}
pcm.plughw {
	@args [ CARD DEV SUBDEV ]
	@args.CARD { type string default 0 }
	@args.DEV { type integer default 0 }
	@args.SUBDEV { type integer default -1 }
	type plug
	slave.pcm {
		type hw
		card $CARD
		device $DEV
		subdevice $SUBDEV
	}
}
pcm.default {
	type plug
	slave.pcm {
		type hw
		card 0
		device 0
	}
}
ctl.hw {
	@args [ CARD ]
	@args.CARD { type string default 0 }
	type hw
	card $CARD
}
ctl.default {
	type hw
	card 0
}
";
