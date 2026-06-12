//! Intel HD Audio (HDA) controller-driver — plan I2, ECHTE audio-uitvoer.
//!
//! De hardware-laag onder de host-geteste [`euroaudio`]-mixer. We praten met de
//! HDA-controller via z'n MMIO-registers, zetten de **CORB/RIRB**-commando-ringen op
//! om de **codec** te enumereren (audio-function-group → DAC + output-pin), routeren
//! een uitvoerpad (converter-format/stream, versterker-gains, pin-output + EAPD), en
//! starten een **stream-DMA** met een BDL (buffer-descriptor-list) die een door
//! [`euroaudio::mix`] gegenereerde toon afspeelt.
//!
//! Verificatie (headless): de codec antwoordt op `GET_PARAMETER` (echte vendor-id via
//! RIRB) en het **stream-positie-register (LPIB) loopt op** terwijl de stream draait —
//! dat bewijst dat de DMA de audio-buffer écht consumeert (= geluid speelt).
//!
//! Alle DMA-structuren (CORB/RIRB/BDL/audiobuffer) komen uit de identity-mapped
//! frame-allocator (virtueel = fysiek), dus de adressen die de controller leest zijn
//! exact dezelfde pointers die wij schrijven.

use euromm::FrameAllocator;

use crate::pci;

// ── Globale controller-registers (MMIO-basis) ──────────────────────────────
const GCAP: u64 = 0x00; // u16: out/in/bidir-stream-counts
const GCTL: u64 = 0x08; // u32: bit0 CRST (controller reset)
const STATESTS: u64 = 0x0E; // u16: bit per codec dat present/changed is
const INTCTL: u64 = 0x20; // u32
const CORBLBASE: u64 = 0x40;
const CORBUBASE: u64 = 0x44;
const CORBWP: u64 = 0x48; // u16
const CORBRP: u64 = 0x4A; // u16
const CORBCTL: u64 = 0x4C; // u8: bit1 DMA-run
const CORBSIZE: u64 = 0x4E; // u8
const RIRBLBASE: u64 = 0x50;
const RIRBUBASE: u64 = 0x54;
const RIRBWP: u64 = 0x58; // u16
const RINTCNT: u64 = 0x5A; // u16
const RIRBCTL: u64 = 0x5C; // u8: bit0 int-on-response, bit1 DMA-run
const RIRBSTS: u64 = 0x5D; // u8: bit0 response-interrupt, bit2 overrun
const RIRBSIZE: u64 = 0x5E; // u8
const SD0: u64 = 0x80; // eerste stream-descriptor (output bij QEMU op index OSS)

// Stream-descriptor-offsets (relatief aan de descriptor-basis).
const SDCTL: u64 = 0x00; // 3 bytes: bit0 SRST, bit1 RUN, bits[23:20] stream-tag
const SDSTS: u64 = 0x03; // u8
const SDLPIB: u64 = 0x04; // u32: link-positie in de buffer
const SDCBL: u64 = 0x08; // u32: cyclic-buffer-length
const SDLVI: u64 = 0x0C; // u16: last-valid-index (BDL-entries − 1)
const SDFMT: u64 = 0x12; // u16: stream-format
const SDBDPL: u64 = 0x18; // u32
const SDBDPU: u64 = 0x1C; // u32

const GCTL_CRST: u32 = 1 << 0;
const CORBRPRST: u16 = 1 << 15;
const DMA_RUN: u8 = 1 << 1;

// ── MMIO-helpers (identity-mapped fysiek) ──────────────────────────────────
struct Mmio(u64);
impl Mmio {
    #[inline]
    unsafe fn r8(&self, o: u64) -> u8 {
        ((self.0 + o) as *const u8).read_volatile()
    }
    #[inline]
    unsafe fn w8(&self, o: u64, v: u8) {
        ((self.0 + o) as *mut u8).write_volatile(v);
    }
    #[inline]
    unsafe fn r16(&self, o: u64) -> u16 {
        ((self.0 + o) as *const u16).read_volatile()
    }
    #[inline]
    unsafe fn w16(&self, o: u64, v: u16) {
        ((self.0 + o) as *mut u16).write_volatile(v);
    }
    #[inline]
    unsafe fn r32(&self, o: u64) -> u32 {
        ((self.0 + o) as *const u32).read_volatile()
    }
    #[inline]
    unsafe fn w32(&self, o: u64, v: u32) {
        ((self.0 + o) as *mut u32).write_volatile(v);
    }
}

struct Hda {
    m: Mmio,
    corb: u64,
    rirb: u64,
    sd: u64, // basis van de gebruikte output-stream-descriptor
    audio: u64,        // adres van de cyclische audio-buffer (voor earcons)
    audio_bytes: usize, // grootte ervan
}

static mut HDA: Option<Hda> = None;

/// Korte busy-wait (de HDA-reset/codec-detectie vraagt µs-pauzes).
fn delay() {
    crate::apic::busy_wait_us(100);
}

/// Stuur één codec-verb via CORB en lees het RIRB-antwoord (32-bit). `v20` is het
/// 20-bit verb-payload-veld; we prefixen codec-adres + node-id.
unsafe fn corb_cmd(h: &Hda, cad: u32, nid: u32, v20: u32) -> u32 {
    let verb = (cad << 28) | (nid << 20) | (v20 & 0x0F_FFFF);
    let wp = h.m.r16(CORBWP) & 0xFF;
    let next = (wp + 1) & 0xFF;
    // CORB is een ring van u32-verbs; schrijf op de volgende index en bump WP.
    ((h.corb + next as u64 * 4) as *mut u32).write_volatile(verb);
    h.m.w16(CORBWP, next);
    // Wacht tot de RIRB-write-pointer meekomt (controller heeft het antwoord geschreven).
    for _ in 0..2_000_000 {
        if (h.m.r16(RIRBWP) & 0xFF) == next {
            break;
        }
        core::hint::spin_loop();
    }
    // Korte settle zodat de respons-DWORD écht in geheugen staat vóór we 'm lezen
    // (de WP-update en de data-write zijn aparte DMA-stappen).
    for _ in 0..2000 {
        core::hint::spin_loop();
    }
    // RIRB-entry = 8 byte (respons-u32 + extended-u32); we willen de respons.
    let resp = ((h.rirb + next as u64 * 8) as *const u32).read_volatile();
    // RIRBSTS.RINTFL (bit0) clearen → reset de RIRB-response-teller, anders stalt de
    // controller de CORB-DMA zodra die teller RINTCNT bereikt (write-1-to-clear).
    h.m.w8(RIRBSTS, 0x05);
    resp
}

#[inline]
unsafe fn get_param(h: &Hda, cad: u32, nid: u32, param: u32) -> u32 {
    corb_cmd(h, cad, nid, (0xF00 << 8) | param)
}
/// 12-bit verb (0xF0x/0x70x) + 8-bit payload.
#[inline]
unsafe fn verb12(h: &Hda, cad: u32, nid: u32, verb: u32, payload: u32) -> u32 {
    corb_cmd(h, cad, nid, (verb << 8) | (payload & 0xFF))
}
/// 4-bit verb (0x2/0x3/...) + 16-bit payload.
#[inline]
unsafe fn verb4(h: &Hda, cad: u32, nid: u32, verb: u32, payload: u32) -> u32 {
    corb_cmd(h, cad, nid, (verb << 16) | (payload & 0xFFFF))
}

/// Detecteer + initialiseer de HDA-controller, enumereer de codec en speel een
/// door [`euroaudio::mix`] gegenereerde toon af (stream-DMA).
pub fn init(falloc: &mut FrameAllocator) -> bool {
    let dev = match pci::find(|d| d.class == 0x04 && d.subclass == 0x03) {
        Some(d) => d,
        None => {
            crate::serial_println!("[hda] geen HD-Audio-controller gevonden (PCI 04:03)");
            return false;
        }
    };
    let bar0 = dev.bar(0);
    let mmio = if bar0 & 0x6 == 0x4 {
        ((dev.bar(1) as u64) << 32) | (bar0 as u64 & 0xFFFF_FFF0)
    } else {
        bar0 as u64 & 0xFFFF_FFF0
    };
    if mmio == 0 {
        crate::serial_println!("[hda] BAR0 niet toegewezen");
        return false;
    }
    dev.enable(0x6); // memory-space + bus-master

    unsafe {
        let m = Mmio(mmio);
        // 1. Controller-reset: CRST=0 (in reset), wacht tot 0; dan CRST=1, wacht tot 1.
        m.w32(GCTL, m.r32(GCTL) & !GCTL_CRST);
        for _ in 0..100_000 {
            if m.r32(GCTL) & GCTL_CRST == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        delay();
        m.w32(GCTL, m.r32(GCTL) | GCTL_CRST);
        for _ in 0..1_000_000 {
            if m.r32(GCTL) & GCTL_CRST != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        delay();
        delay(); // codecs tijd geven om in STATESTS te verschijnen

        let gcap = m.r16(GCAP);
        let iss = ((gcap >> 8) & 0x0F) as u64; // input-streams (output begint erna)
        let statests = m.r16(STATESTS);
        crate::serial_println!(
            "[hda] controller @ {:#x} — GCAP={:#06x} (in={} out={} bidir={}), STATESTS={:#x}",
            mmio, gcap, iss, (gcap >> 12) & 0x0F, (gcap >> 3) & 0x0F, statests
        );
        if statests == 0 {
            crate::serial_println!("[hda] geen codec gedetecteerd");
            return false;
        }
        let cad = statests.trailing_zeros() as u32; // eerste aanwezige codec-adres

        // 2. CORB opzetten (256 verbs = 1 KiB). Stop DMA, basis zetten, RP resetten.
        let corb = falloc.allocate().expect("hda corb");
        let rirb = falloc.allocate().expect("hda rirb");
        core::ptr::write_bytes(corb as *mut u8, 0, 4096);
        core::ptr::write_bytes(rirb as *mut u8, 0, 4096);
        m.w8(CORBCTL, 0);
        m.w8(RIRBCTL, 0);
        m.w8(CORBSIZE, 0x02); // 256 entries
        m.w32(CORBLBASE, corb as u32);
        m.w32(CORBUBASE, (corb >> 32) as u32);
        // CORB-read-pointer resetten.
        m.w16(CORBRP, CORBRPRST);
        for _ in 0..100_000 {
            if m.r16(CORBRP) & CORBRPRST != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        m.w16(CORBRP, 0);
        m.w16(CORBWP, 0);
        // 3. RIRB opzetten.
        m.w8(RIRBSIZE, 0x02); // 256 entries
        m.w32(RIRBLBASE, rirb as u32);
        m.w32(RIRBUBASE, (rirb >> 32) as u32);
        m.w16(RIRBWP, 0x8000); // write-pointer resetten
        m.w16(RINTCNT, 0xFF); // hoog → CORB-DMA stalt niet tijdens de enumeratie
        m.w8(RIRBCTL, DMA_RUN); // RIRB-DMA aan
        m.w8(CORBCTL, DMA_RUN); // CORB-DMA aan
        m.w32(INTCTL, 0); // we pollen (geen interrupts)
        delay();

        let mut h = Hda { m, corb, rirb, sd: SD0 + iss * 0x20, audio: 0, audio_bytes: 0 };

        // 4. Codec enumereren: vendor-id + function-groups.
        let vid = get_param(&h, cad, 0, 0x00); // VENDOR_ID
        let mut snc = get_param(&h, cad, 0, 0x04); // SUBORDINATE_NODE_COUNT van root
        if snc & 0xFF == 0 {
            delay();
            snc = get_param(&h, cad, 0, 0x04); // één retry na een settle
        }
        let fg_start = (snc >> 16) & 0xFF;
        let fg_count = snc & 0xFF;
        crate::serial_println!(
            "[hda] codec #{cad}: vendor-id={:#010x}, {} function-group(s) vanaf node {}",
            vid, fg_count, fg_start
        );

        // Zoek de audio-function-group → eerste DAC (output-converter) + output-pin.
        let mut dac = 0u32;
        let mut pin = 0u32;
        for fg in fg_start..fg_start + fg_count {
            let fgt = get_param(&h, cad, fg, 0x05) & 0xFF; // FUNCTION_GROUP_TYPE
            if fgt != 0x01 {
                continue; // 0x01 = audio-function-group
            }
            let wsnc = get_param(&h, cad, fg, 0x04);
            let w_start = (wsnc >> 16) & 0xFF;
            let w_count = wsnc & 0xFF;
            for w in w_start..w_start + w_count {
                let cap = get_param(&h, cad, w, 0x09); // AUDIO_WIDGET_CAP
                let wtype = (cap >> 20) & 0x0F;
                if wtype == 0x0 && dac == 0 {
                    dac = w; // output-converter (DAC)
                } else if wtype == 0x4 && pin == 0 {
                    pin = w; // pin-complex
                }
            }
            if dac != 0 {
                break;
            }
        }
        if dac == 0 {
            crate::serial_println!("[hda] geen output-converter (DAC) gevonden");
            return false;
        }
        crate::serial_println!("[hda] uitvoerpad: DAC=node {dac}, pin=node {pin}");

        // 5. Audio-buffer: meng twee blok-toon-streams via euroaudio → één PCM-buffer.
        //    8 aaneengesloten frames = 32 KiB = 8192 i16-stereo-samples.
        const FRAMES: usize = 8;
        let audio = falloc.allocate().expect("hda audio-buf");
        for _ in 1..FRAMES {
            falloc.allocate().expect("hda audio-buf-cont");
        }
        let total_bytes = FRAMES * 4096;
        let nsamp = total_bytes / 2; // i16-samples (stereo interleaved)
        let tone = build_tone(nsamp);
        core::ptr::copy_nonoverlapping(tone.as_ptr() as *const u8, audio as *mut u8, total_bytes);
        h.audio = audio;
        h.audio_bytes = total_bytes;

        // 6. BDL: 2 entries (elk de helft), beide met IOC. CBL = totale lengte.
        let bdl = falloc.allocate().expect("hda bdl");
        core::ptr::write_bytes(bdl as *mut u8, 0, 4096);
        let half = (total_bytes / 2) as u32;
        for i in 0..2u64 {
            let e = bdl + i * 16;
            let addr = audio + i * half as u64;
            (e as *mut u64).write_volatile(addr);
            ((e + 8) as *mut u32).write_volatile(half);
            ((e + 12) as *mut u32).write_volatile(1); // IOC
        }

        // 7. Codec-pad configureren. Format: 48 kHz, 16-bit, 2 kanalen = 0x0011.
        const FMT: u32 = 0x0011;
        verb4(&h, cad, dac, 0x2, FMT); // SET_CONVERTER_FORMAT
        verb12(&h, cad, dac, 0x706, 1 << 4); // SET_CONVERTER_STREAM_CHANNEL: stream 1, chan 0
        verb4(&h, cad, dac, 0x3, 0xB000 | 0x2A); // SET_AMP_GAIN_MUTE (output, L+R, unmute, gain)
        if pin != 0 {
            verb12(&h, cad, pin, 0x707, 0x40); // SET_PIN_WIDGET_CONTROL: output enable
            verb12(&h, cad, pin, 0x70C, 0x2); // SET_EAPD_BTL_ENABLE: EAPD aan
            verb4(&h, cad, pin, 0x3, 0xB000 | 0x2A); // pin output-amp unmute
        }

        // 8. Stream-descriptor opzetten + starten.
        let sd = h.sd;
        // Reset de stream (SRST).
        h.m.w8(sd + SDCTL, 0x01);
        for _ in 0..100_000 {
            if h.m.r8(sd + SDCTL) & 0x01 != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        h.m.w8(sd + SDCTL, 0x00);
        for _ in 0..100_000 {
            if h.m.r8(sd + SDCTL) & 0x01 == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        h.m.w32(sd + SDCBL, total_bytes as u32);
        h.m.w16(sd + SDLVI, 1); // 2 BDL-entries
        h.m.w16(sd + SDFMT, FMT as u16);
        h.m.w32(sd + SDBDPL, bdl as u32);
        h.m.w32(sd + SDBDPU, (bdl >> 32) as u32);
        h.m.w8(sd + SDSTS, 0x1C); // status-bits clearen (write-1-clear)
        // Stream-tag 1 in bits[23:20] + RUN (bit1).
        let ctl = (1u32 << 20) | (DMA_RUN as u32);
        h.m.w8(sd + SDCTL + 2, (ctl >> 16) as u8);
        h.m.w8(sd + SDCTL, (ctl & 0xFF) as u8);

        // 9. Verifieer dat de DMA de buffer consumeert: LPIB moet oplopen. QEMU's
        //    audio-timer tikt op wandkloktijd, dus pollen we tot ~250 ms (guest) op
        //    beweging i.p.v. één momentopname.
        let p0 = h.m.r32(sd + SDLPIB);
        let mut p1 = p0;
        for _ in 0..2500 {
            delay(); // ~100 µs elk → tot ~250 ms
            p1 = h.m.r32(sd + SDLPIB);
            if p1 != p0 {
                break;
            }
        }
        let sdctl = h.m.r8(sd + SDCTL);
        let sdsts = h.m.r8(sd + SDSTS);
        crate::serial_println!(
            "[hda] stream gestart (tag 1, 48 kHz/16-bit/stereo, {} KiB) — SDCTL={:#x} SDSTS={:#x} LPIB {} → {} ({})",
            total_bytes / 1024, sdctl, sdsts, p0, p1,
            if p1 != p0 { "DMA loopt → audio speelt ✓" } else { "geen voortgang" }
        );

        HDA = Some(h);
        p1 != p0
    }
}

/// Genereer een 48 kHz 16-bit STEREO PCM-toon door twee blokgolven (A4 440 Hz +
/// E5 660 Hz) via [`euroaudio::mix`] te mengen — bewijst de mixer→hardware-keten.
fn build_tone(nsamples: usize) -> alloc::vec::Vec<i16> {
    let frames = nsamples / 2; // stereo
    let sr = 48_000u32;
    let amp = 6000i16;
    let sq = |hz: u32, n: usize| -> alloc::vec::Vec<i16> {
        let period = (sr / hz) as usize;
        (0..n)
            .map(|i| if (i % period) < period / 2 { amp } else { -amp })
            .collect()
    };
    let a = sq(440, frames);
    let b = sq(660, frames);
    let mut mono = alloc::vec![0i16; frames];
    // Mix beide tonen op halve volume (geen clip) via de host-geteste mixer.
    euroaudio::mix(&[(&a, euroaudio::UNITY / 2), (&b, euroaudio::UNITY / 2)], &mut mono);
    // Mono → stereo interleave.
    let mut out = alloc::vec![0i16; nsamples];
    for (i, &s) in mono.iter().enumerate() {
        out[i * 2] = s;
        out[i * 2 + 1] = s;
    }
    out
}

/// Is er een HDA-controller geïnitialiseerd?
pub fn present() -> bool {
    unsafe { (*core::ptr::addr_of!(HDA)).is_some() }
}

/// Huidige stream-positie (LPIB) — diagnostiek/zelftest.
pub fn stream_pos() -> u32 {
    unsafe {
        match (*core::ptr::addr_of!(HDA)).as_ref() {
            Some(h) => h.m.r32(h.sd + SDLPIB),
            None => 0,
        }
    }
}

/// Draait de output-stream-DMA? (SDCTL bit1 RUN) — betrouwbaarder dan een
/// LPIB-momentopname, want QEMU's audio-DMA tikt traag zonder `-audiodev`.
pub fn stream_running() -> bool {
    unsafe {
        match (*core::ptr::addr_of!(HDA)).as_ref() {
            Some(h) => h.m.r8(h.sd + SDCTL) & DMA_RUN != 0,
            None => false,
        }
    }
}

/// **BB-8** — speel een korte **earcon** (audio-cue) op `freq_hz`. De stream-DMA
/// loopt cyclisch over de audio-buffer; we schrijven er een korte blokgolf-beep
/// (~125 ms) + stilte in, zodat een ONDERSCHEIDENDE toon klinkt per schermlezer-
/// event (knop/selectievakje/tekstveld krijgen elk hun eigen toonhoogte). No-op
/// als er geen HDA-controller is. Geeft true als de buffer (her)beschreven is.
pub fn earcon(freq_hz: u32) -> bool {
    unsafe {
        let h = match (*core::ptr::addr_of!(HDA)).as_ref() {
            Some(h) => h,
            None => return false,
        };
        if h.audio == 0 || freq_hz == 0 {
            return false;
        }
        let frames = (h.audio_bytes / 2) / 2; // stereo-frames
        let beep_frames = frames.min(6000); // ~125 ms @ 48 kHz
        let half_period = (24_000 / freq_hz.max(1)).max(1) as usize;
        let buf = h.audio as *mut i16;
        let mut level: i16 = 9000;
        for fr in 0..frames {
            let s = if fr < beep_frames {
                if fr % half_period == 0 {
                    level = -level;
                }
                level
            } else {
                0 // stilte na de beep → het is een korte cue, geen drone
            };
            core::ptr::write_volatile(buf.add(fr * 2), s); // L
            core::ptr::write_volatile(buf.add(fr * 2 + 1), s); // R
        }
        true
    }
}
