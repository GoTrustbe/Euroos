//! H4: EuroWASM-integratie — draai een WASM-module in de kernel via de no-JIT
//! interpreter, met de WASI-imports afgebeeld op **EuroGuard-capabilities**. De
//! zelftest bouwt een module die (a) een loop-som 1..=10 = 55 berekent (bewijst de
//! interpreter: locals, loop, br_if, rekenkunde) en (b) `euro.fd_write` aanroept om
//! een bericht te schrijven — dat alleen slaagt als de host de capability verleent.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use eurofs::FileSystem;
use eurowasm::{HostImports, Instance, Module, Val, WasmError};

// ── Mini-assembler voor de demo-module (zelfde codering als de host-tests) ──
fn uleb(mut n: u32) -> Vec<u8> {
    let mut o = Vec::new();
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n != 0 {
            o.push(b | 0x80);
        } else {
            o.push(b);
            break;
        }
    }
    o
}
fn section(id: u8, content: Vec<u8>) -> Vec<u8> {
    let mut s = vec![id];
    s.extend(uleb(content.len() as u32));
    s.extend(content);
    s
}
fn sleb(mut v: i64) -> Vec<u8> {
    let mut o = Vec::new();
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        let sign = b & 0x40;
        if (v == 0 && sign == 0) || (v == -1 && sign != 0) {
            o.push(b);
            break;
        }
        o.push(b | 0x80);
    }
    o
}

/// Het bericht dat de WASM-module via `fd_write` schrijft. De modulecode geeft de
/// lengte mee; de kernel legt het in het lineair geheugen vóór de aanroep.
const MSG: &[u8] = b"EuroWASM: no-JIT interpreter draait in de kernel\n";

/// Bouw de demo-module: importeert `euro.fd_write`, exporteert `run()->i32` die
/// `fd_write(0, MSG.len())` aanroept en daarna de loop-som 1..=10 = 55 teruggeeft.
fn build_module() -> Vec<u8> {
    let mut w = vec![0u8, 0x61, 0x73, 0x6d, 1, 0, 0, 0]; // \0asm + versie
    // types: 0 = (i32,i32)->i32 (fd_write), 1 = ()->i32 (run)
    w.extend(section(1, vec![2, 0x60, 2, 0x7f, 0x7f, 1, 0x7f, 0x60, 0, 1, 0x7f]));
    // import euro.fd_write : type 0
    let mut im = vec![1u8, 4];
    im.extend_from_slice(b"euro");
    im.push(8);
    im.extend_from_slice(b"fd_write");
    im.extend_from_slice(&[0x00, 0]);
    w.extend(section(2, im));
    w.extend(section(3, vec![1, 1])); // 1 functie, type 1
    w.extend(section(5, vec![1, 0x00, 1])); // 1 mem-pagina
    let mut ex = vec![1u8, 3];
    ex.extend_from_slice(b"run");
    ex.extend_from_slice(&[0x00, 1]); // export "run" = functie-index 1 (import 0 + def 0)
    w.extend(section(7, ex));
    // code: fd_write(0, len); drop; som 1..=10; end. Locals: i(0), acc(1).
    let len = MSG.len() as u8; // < 128 → één sleb-byte
    let mut body = vec![1u8, 2, 0x7f]; // 1 local-groep: 2× i32
    body.extend_from_slice(&[
        0x41, 0x00, 0x41, len, 0x10, 0x00, 0x1a, // fd_write(0,len); drop
        0x41, 0x00, 0x21, 0x01, // acc = 0
        0x41, 0x01, 0x21, 0x00, // i = 1
        0x02, 0x40, // block
        0x03, 0x40, // loop
        0x20, 0x00, 0x41, 0x0a, 0x4a, 0x0d, 0x01, // if i>10: br 1 (exit block)
        0x20, 0x01, 0x20, 0x00, 0x6a, 0x21, 0x01, // acc += i
        0x20, 0x00, 0x41, 0x01, 0x6a, 0x21, 0x00, // i += 1
        0x0c, 0x00, // br 0 (loop)
        0x0b, 0x0b, // end loop, end block
        0x20, 0x01, // local.get acc
        0x0b, // end func
    ]);
    let mut code = vec![1u8];
    code.extend(uleb(body.len() as u32));
    code.extend(body);
    w.extend(section(10, code));
    w
}

/// Een host die `euro.fd_write` op een EuroGuard-capability afbeeldt: zonder de
/// capability wordt de call geweigerd (de WASM-trap propageert). Mét de capability
/// schrijft hij de bytes (hier: verzamelt ze + logt naar serial).
struct CapHost {
    cap_console: bool,
    out: Vec<u8>,
}
impl HostImports for CapHost {
    fn call(&mut self, m: &str, n: &str, args: &[Val], mem: &mut [u8]) -> Result<Vec<Val>, WasmError> {
        if m == "euro" && n == "fd_write" {
            if !self.cap_console {
                return Err(WasmError::CapabilityDenied(alloc::string::String::from(
                    "CAP_CONSOLE voor fd_write",
                )));
            }
            let ptr = args[0] as usize;
            let len = args[1] as usize;
            if ptr + len <= mem.len() {
                self.out.extend_from_slice(&mem[ptr..ptr + len]);
            }
            return Ok(vec![len as i64]);
        }
        Err(WasmError::HostError(alloc::string::String::from("onbekende import")))
    }
}

/// H4-zelftest: parse + draai de WASM-module met EN zonder de capability.
pub fn selftest() {
    let bytes = build_module();
    let module = match Module::parse(&bytes) {
        Ok(m) => m,
        Err(e) => {
            crate::serial_println!("[h4] WASM-parse mislukt: {:?}", e);
            return;
        }
    };

    // (1) MÉT CAP_CONSOLE: fd_write slaagt, de som komt terug.
    let mut inst = Instance::new(&module);
    let _ = inst.write_mem(0, MSG); // kernel legt het bericht in WASM-geheugen
    let mut host = CapHost { cap_console: true, out: Vec::new() };
    match inst.invoke("run", &[], &mut host) {
        Ok(r) => {
            let sum = r.first().copied().unwrap_or(-1);
            crate::serial_println!(
                "[h4] WASM run() = {} (verwacht 55), fd_write→capability schreef {} bytes: {:?}",
                sum,
                host.out.len(),
                core::str::from_utf8(&host.out).unwrap_or("?").trim_end()
            );
        }
        Err(e) => crate::serial_println!("[h4] WASM-run mislukt: {:?}", e),
    }

    // (2) ZONDER de capability: de WASI-import wordt geweigerd (sandbox-grens).
    let mut inst2 = Instance::new(&module);
    let _ = inst2.write_mem(0, MSG);
    let mut deny = CapHost { cap_console: false, out: Vec::new() };
    match inst2.invoke("run", &[], &mut deny) {
        Ok(_) => crate::serial_println!("[h4] FOUT: fd_write zonder capability had geweigerd moeten worden"),
        Err(WasmError::CapabilityDenied(c)) => {
            crate::serial_println!("[h4] WASI-capability-poort: fd_write GEWEIGERD zonder {} ✓", c)
        }
        Err(e) => crate::serial_println!("[h4] onverwachte fout: {:?}", e),
    }
}

// ── AH-3: een ZELF-DRAGENDE .wasm (eigen data-sectie) + `wasm <bestand>` ──────

/// Zoals [`build_module`], maar met een **data-sectie** zodat het bericht ín de
/// module zit (geen kernel-`write_mem` meer nodig) — een echte, zelf-dragende
/// `.wasm` die van schijf geladen en gedraaid kan worden.
fn build_demo_wasm() -> Vec<u8> {
    let mut w = build_module();
    // data: 1 actief segment op offset 0 met MSG (de loader/host hoeft niets te injecteren).
    let mut d = vec![1u8, 0u8]; // 1 segment, flags 0 (actief, mem 0)
    d.extend_from_slice(&[0x41, 0x00, 0x0b]); // offset = i32.const 0, end
    d.extend(uleb(MSG.len() as u32));
    d.extend_from_slice(MSG);
    w.extend(section(11, d));
    w
}

/// Voer een ECHTE `.wasm` uit het VFS uit (H4-remainder: `wasm <bestand>`), in de
/// no-JIT sandbox met cap-gated WASI. De gebruiker draaide het zelf → console-cap
/// verleend; de sandbox-grens blijft (een niet-verleende host-call trapt).
pub fn run_file(fs: &mut dyn FileSystem, path: &str) -> Vec<String> {
    match fs.read_file(path) {
        Ok(bytes) => run_bytes(&bytes, path, true),
        Err(_) => vec![format!("wasm: {path}: bestand niet gevonden")],
    }
}

/// Parse + draai WASM-bytes; `cap` = of de console-capability verleend is.
fn run_bytes(bytes: &[u8], label: &str, cap: bool) -> Vec<String> {
    let module = match Module::parse(bytes) {
        Ok(m) => m,
        Err(e) => return vec![format!("wasm: {label}: parse-fout {:?}", e)],
    };
    let entry = match ["run", "_start", "main"].into_iter().find(|e| module.has_export(e)) {
        Some(e) => e,
        None => return vec![format!("wasm: {label}: geen 'run'/'_start'/'main'-export")],
    };
    let mut inst = Instance::new(&module);
    let mut host = CapHost { cap_console: cap, out: Vec::new() };
    match inst.invoke(entry, &[], &mut host) {
        Ok(r) => {
            let mut out = Vec::new();
            if !host.out.is_empty() {
                out.push(format!("  [fd_write] {}", String::from_utf8_lossy(&host.out).trim_end()));
            }
            out.push(format!(
                "wasm: {label} · {}() = {} · no-JIT sandbox, WASI cap-gated",
                entry,
                r.first().copied().unwrap_or(0)
            ));
            out
        }
        Err(WasmError::CapabilityDenied(c)) => vec![format!("wasm: {label}: host-call GEWEIGERD ({c}) — sandbox-grens")],
        Err(e) => vec![format!("wasm: {label}: trap {:?}", e)],
    }
}

/// `[wasm2]`-zelftest: schrijf een echte zelf-dragende `.wasm` naar EuroFS en draai
/// ze via het `wasm <bestand>`-pad — mét cap (output + som) en zonder (geweigerd).
pub fn selftest_file(fs: &mut dyn FileSystem) {
    let bytes = build_demo_wasm();
    let _ = fs.create_dir("/agents");
    let _ = fs.write_file("/agents/demo.wasm", &bytes);
    let on_fs = fs.read_file("/agents/demo.wasm").map(|d| d == bytes).unwrap_or(false);

    let granted = run_bytes(&bytes, "/agents/demo.wasm", true);
    let msg_ok = granted.iter().any(|l| l.contains("EuroWASM"));
    let sum_ok = granted.iter().any(|l| l.contains("= 55"));
    let denied = run_bytes(&bytes, "/agents/demo.wasm", false);
    let denied_ok = denied.iter().any(|l| l.contains("GEWEIGERD"));

    let ok = on_fs && msg_ok && sum_ok && denied_ok;
    crate::serial_println!(
        "[wasm2] `wasm <bestand>`: zelf-dragende .wasm (data-sectie) op EuroFS={on_fs}, mét-cap fd_write+run()=55={}, zonder-cap host-call-geweigerd={denied_ok} → {}",
        msg_ok && sum_ok,
        if ok { "OK (echte .wasm van schijf in de no-JIT sandbox, WASI cap-gated) ✓" } else { "MISLUKT" }
    );
}

// ── H4-vervolg: bind de WASM-WASI aan een ECHTE EuroSandbox-container ───────
// De WASI-imports worden gepoort op de container z'n EFFECTIEVE capabilities +
// netwerk-scope (EuroGuard). Zo wordt een WASM-app daadwerkelijk door het
// soevereine capability-model bestuurd, niet door een test-vlag.
use crate::ring3::{CAP_CONSOLE, CAP_FILE, CAP_NET};
use eurosandbox::{Container, NetScope};

/// Een WASI-host gebonden aan één container: elke host-call wordt getoetst aan de
/// EFFECTIEVE capabilities (`base ∩ container.caps`) en, voor netwerk, de net-scope.
struct ContainerWasiHost<'c> {
    c: &'c Container,
    base: u64,
}
impl HostImports for ContainerWasiHost<'_> {
    fn call(&mut self, m: &str, n: &str, args: &[Val], mem: &mut [u8]) -> Result<Vec<Val>, WasmError> {
        let eff = self.c.effective_caps(self.base);
        match (m, n) {
            ("euro", "fd_write") => {
                if eff & CAP_CONSOLE == 0 {
                    return Err(WasmError::CapabilityDenied(alloc::string::String::from("CAP_CONSOLE")));
                }
                let (ptr, len) = (args[0] as usize, args[1] as usize);
                if ptr + len <= mem.len() {
                    crate::serial_println!("[h4-ctr]   wasi fd_write: {:?}", core::str::from_utf8(&mem[ptr..ptr + len]).unwrap_or("?"));
                }
                Ok(vec![len as i64])
            }
            ("euro", "sock_connect") => {
                if eff & CAP_NET == 0 {
                    return Err(WasmError::CapabilityDenied(alloc::string::String::from("CAP_NET")));
                }
                let v = args[0] as u32;
                let ip = [(v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8];
                let port = args[1] as u16;
                if !self.c.allow_connect(ip, port) {
                    return Err(WasmError::CapabilityDenied(alloc::format!(
                        "netwerk-scope verbiedt {}.{}.{}.{}:{}",
                        ip[0], ip[1], ip[2], ip[3], port
                    )));
                }
                Ok(vec![1])
            }
            _ => Err(WasmError::HostError(alloc::string::String::from("onbekende import"))),
        }
    }
}

/// Module die `euro.sock_connect(ip, port)` importeert en `try_net()->i32` exporteert
/// die 10.0.2.2:80 probeert te bereiken — gateway voor de container-test.
fn build_net_module() -> Vec<u8> {
    let mut w = vec![0u8, 0x61, 0x73, 0x6d, 1, 0, 0, 0];
    w.extend(section(1, vec![2, 0x60, 2, 0x7f, 0x7f, 1, 0x7f, 0x60, 0, 1, 0x7f]));
    let mut im = vec![1u8, 4];
    im.extend_from_slice(b"euro");
    im.push(12);
    im.extend_from_slice(b"sock_connect");
    im.extend_from_slice(&[0x00, 0]);
    w.extend(section(2, im));
    w.extend(section(3, vec![1, 1]));
    let mut ex = vec![1u8, 7];
    ex.extend_from_slice(b"try_net");
    ex.extend_from_slice(&[0x00, 1]);
    w.extend(section(7, ex));
    let ip = (10i64 << 24) | (2 << 8) | 2; // 10.0.2.2 big-endian gepakt
    let mut body = vec![0u8]; // geen locals
    body.push(0x41);
    body.extend(sleb(ip));
    body.push(0x41);
    body.extend(sleb(80)); // poort 80
    body.extend_from_slice(&[0x10, 0x00, 0x0b]); // call 0 (sock_connect); end
    let mut code = vec![1u8];
    code.extend(uleb(body.len() as u32));
    code.extend(body);
    w.extend(section(10, code));
    w
}

/// H4-vervolgzelftest: draai dezelfde WASM-module in DRIE EuroSandbox-containers en
/// laat zien dat de WASI-`sock_connect` door de container-capabilities + net-scope
/// wordt bestuurd: toegestaan, geweigerd-zonder-CAP_NET, geweigerd-door-scope.
pub fn container_selftest() {
    let bytes = build_net_module();
    let module = match Module::parse(&bytes) {
        Ok(m) => m,
        Err(e) => {
            crate::serial_println!("[h4-ctr] parse mislukt: {:?}", e);
            return;
        }
    };
    let base = CAP_CONSOLE | CAP_NET | CAP_FILE; // proces-basismasker
    let ok = Container::new("net-ok", base, NetScope::Allow(vec![([10, 0, 2, 2], 80)]));
    let no_net = Container::new("no-net", CAP_CONSOLE, NetScope::Any); // CAP_NET ontnomen
    let scoped = Container::new("scoped", base, NetScope::Allow(vec![([1, 1, 1, 1], 443)]));
    for ctr in [&ok, &no_net, &scoped] {
        let mut inst = Instance::new(&module);
        let mut host = ContainerWasiHost { c: ctr, base };
        match inst.invoke("try_net", &[], &mut host) {
            Ok(v) => crate::serial_println!(
                "[h4-ctr] container '{}' (caps {:#06b}): sock_connect TOEGESTAAN → {:?}",
                ctr.name,
                ctr.effective_caps(base),
                v
            ),
            Err(WasmError::CapabilityDenied(why)) => crate::serial_println!(
                "[h4-ctr] container '{}' (caps {:#06b}): sock_connect GEWEIGERD — {} ✓",
                ctr.name,
                ctr.effective_caps(base),
                why
            ),
            Err(e) => crate::serial_println!("[h4-ctr] container '{}': fout {:?}", ctr.name, e),
        }
    }
}
