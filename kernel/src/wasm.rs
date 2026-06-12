//! H4: EuroWASM-integratie — draai een WASM-module in de kernel via de no-JIT
//! interpreter, met de WASI-imports afgebeeld op **EuroGuard-capabilities**. De
//! zelftest bouwt een module die (a) een loop-som 1..=10 = 55 berekent (bewijst de
//! interpreter: locals, loop, br_if, rekenkunde) en (b) `euro.fd_write` aanroept om
//! een bericht te schrijven — dat alleen slaagt als de host de capability verleent.

use alloc::vec;
use alloc::vec::Vec;
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
