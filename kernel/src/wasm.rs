//! H4: EuroWASM integration — run a WASM module in the kernel via the no-JIT
//! interpreter, with the WASI imports mapped onto **EuroGuard capabilities**. The
//! self-test builds a module that (a) computes a loop sum 1..=10 = 55 (proving the
//! interpreter: locals, loop, br_if, arithmetic) and (b) calls `euro.fd_write` to
//! write a message — which only succeeds if the host grants the capability.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use eurofs::FileSystem;
use eurowasm::{HostImports, Instance, Module, Val, WasmError};

// ── Mini-assembler for the demo module (same encoding as the host tests) ──
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

/// The message the WASM module writes via `fd_write`. The module code passes the
/// length; the kernel places it in linear memory before the call.
const MSG: &[u8] = b"EuroWASM: no-JIT interpreter runs in the kernel\n";

/// Build the demo module: imports `euro.fd_write`, exports `run()->i32` which
/// calls `fd_write(0, MSG.len())` and then returns the loop sum 1..=10 = 55.
fn build_module() -> Vec<u8> {
    let mut w = vec![0u8, 0x61, 0x73, 0x6d, 1, 0, 0, 0]; // \0asm + version
    // types: 0 = (i32,i32)->i32 (fd_write), 1 = ()->i32 (run)
    w.extend(section(1, vec![2, 0x60, 2, 0x7f, 0x7f, 1, 0x7f, 0x60, 0, 1, 0x7f]));
    // import euro.fd_write : type 0
    let mut im = vec![1u8, 4];
    im.extend_from_slice(b"euro");
    im.push(8);
    im.extend_from_slice(b"fd_write");
    im.extend_from_slice(&[0x00, 0]);
    w.extend(section(2, im));
    w.extend(section(3, vec![1, 1])); // 1 function, type 1
    w.extend(section(5, vec![1, 0x00, 1])); // 1 mem page
    let mut ex = vec![1u8, 3];
    ex.extend_from_slice(b"run");
    ex.extend_from_slice(&[0x00, 1]); // export "run" = function index 1 (import 0 + def 0)
    w.extend(section(7, ex));
    // code: fd_write(0, len); drop; sum 1..=10; end. Locals: i(0), acc(1).
    let len = MSG.len() as u8; // < 128 → one sleb byte
    let mut body = vec![1u8, 2, 0x7f]; // 1 local group: 2× i32
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

/// A host that maps `euro.fd_write` onto an EuroGuard capability: without the
/// capability the call is denied (the WASM trap propagates). With the capability
/// it writes the bytes (here: collects them + logs to serial).
struct CapHost {
    cap_console: bool,
    out: Vec<u8>,
}
impl HostImports for CapHost {
    fn call(&mut self, m: &str, n: &str, args: &[Val], mem: &mut [u8]) -> Result<Vec<Val>, WasmError> {
        if m == "euro" && n == "fd_write" {
            if !self.cap_console {
                return Err(WasmError::CapabilityDenied(alloc::string::String::from(
                    "CAP_CONSOLE for fd_write",
                )));
            }
            let ptr = args[0] as usize;
            let len = args[1] as usize;
            if ptr + len <= mem.len() {
                self.out.extend_from_slice(&mem[ptr..ptr + len]);
            }
            return Ok(vec![len as i64]);
        }
        Err(WasmError::HostError(alloc::string::String::from("unknown import")))
    }
}

/// H4 self-test: parse + run the WASM module WITH and WITHOUT the capability.
pub fn selftest() {
    let bytes = build_module();
    let module = match Module::parse(&bytes) {
        Ok(m) => m,
        Err(e) => {
            crate::serial_println!("[h4] WASM parse failed: {:?}", e);
            return;
        }
    };

    // (1) WITH CAP_CONSOLE: fd_write succeeds, the sum comes back.
    let mut inst = Instance::new(&module);
    let _ = inst.write_mem(0, MSG); // kernel places the message in WASM memory
    let mut host = CapHost { cap_console: true, out: Vec::new() };
    match inst.invoke("run", &[], &mut host) {
        Ok(r) => {
            let sum = r.first().copied().unwrap_or(-1);
            crate::serial_println!(
                "[h4] WASM run() = {} (expected 55), fd_write→capability wrote {} bytes: {:?}",
                sum,
                host.out.len(),
                core::str::from_utf8(&host.out).unwrap_or("?").trim_end()
            );
        }
        Err(e) => crate::serial_println!("[h4] WASM run failed: {:?}", e),
    }

    // (2) WITHOUT the capability: the WASI import is denied (sandbox boundary).
    let mut inst2 = Instance::new(&module);
    let _ = inst2.write_mem(0, MSG);
    let mut deny = CapHost { cap_console: false, out: Vec::new() };
    match inst2.invoke("run", &[], &mut deny) {
        Ok(_) => crate::serial_println!("[h4] ERROR: fd_write without capability should have been denied"),
        Err(WasmError::CapabilityDenied(c)) => {
            crate::serial_println!("[h4] WASI capability gate: fd_write DENIED without {} ✓", c)
        }
        Err(e) => crate::serial_println!("[h4] unexpected error: {:?}", e),
    }
}

// ── AH-3: a SELF-CONTAINED .wasm (own data section) + `wasm <file>` ──────

/// Like [`build_module`], but with a **data section** so the message is IN the
/// module (no kernel `write_mem` needed anymore) — a real, self-contained
/// `.wasm` that can be loaded from disk and run.
fn build_demo_wasm() -> Vec<u8> {
    let mut w = build_module();
    // data: 1 active segment at offset 0 with MSG (the loader/host need not inject anything).
    let mut d = vec![1u8, 0u8]; // 1 segment, flags 0 (active, mem 0)
    d.extend_from_slice(&[0x41, 0x00, 0x0b]); // offset = i32.const 0, end
    d.extend(uleb(MSG.len() as u32));
    d.extend_from_slice(MSG);
    w.extend(section(11, d));
    w
}

/// Run a REAL `.wasm` from the VFS (H4 remainder: `wasm <file>`), in the
/// no-JIT sandbox with cap-gated WASI. The user ran it themselves → console cap
/// granted; the sandbox boundary remains (a non-granted host call traps).
pub fn run_file(fs: &mut dyn FileSystem, path: &str) -> Vec<String> {
    match fs.read_file(path) {
        Ok(bytes) => run_bytes(&bytes, path, true),
        Err(_) => vec![format!("wasm: {path}: file not found")],
    }
}

/// Parse + run WASM bytes; `cap` = whether the console capability is granted.
fn run_bytes(bytes: &[u8], label: &str, cap: bool) -> Vec<String> {
    let module = match Module::parse(bytes) {
        Ok(m) => m,
        Err(e) => return vec![format!("wasm: {label}: parse error {:?}", e)],
    };
    let entry = match ["run", "_start", "main"].into_iter().find(|e| module.has_export(e)) {
        Some(e) => e,
        None => return vec![format!("wasm: {label}: no 'run'/'_start'/'main' export")],
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
        Err(WasmError::CapabilityDenied(c)) => vec![format!("wasm: {label}: host call DENIED ({c}) — sandbox boundary")],
        Err(e) => vec![format!("wasm: {label}: trap {:?}", e)],
    }
}

/// `[wasm2]` self-test: write a real self-contained `.wasm` to EuroFS and run
/// it via the `wasm <file>` path — with cap (output + sum) and without (denied).
pub fn selftest_file(fs: &mut dyn FileSystem) {
    let bytes = build_demo_wasm();
    let _ = fs.create_dir("/agents");
    let _ = fs.write_file("/agents/demo.wasm", &bytes);
    let on_fs = fs.read_file("/agents/demo.wasm").map(|d| d == bytes).unwrap_or(false);

    let granted = run_bytes(&bytes, "/agents/demo.wasm", true);
    let msg_ok = granted.iter().any(|l| l.contains("EuroWASM"));
    let sum_ok = granted.iter().any(|l| l.contains("= 55"));
    let denied = run_bytes(&bytes, "/agents/demo.wasm", false);
    let denied_ok = denied.iter().any(|l| l.contains("DENIED"));

    let ok = on_fs && msg_ok && sum_ok && denied_ok;
    crate::serial_println!(
        "[wasm2] `wasm <file>`: self-contained .wasm (data section) on EuroFS={on_fs}, with-cap fd_write+run()=55={}, without-cap host-call-denied={denied_ok} → {}",
        msg_ok && sum_ok,
        if ok { "OK (real .wasm from disk in the no-JIT sandbox, WASI cap-gated) ✓" } else { "FAILED" }
    );
}

// ── H4 follow-up: bind the WASM WASI to a REAL EuroSandbox container ───────
// The WASI imports are gated on the container's EFFECTIVE capabilities +
// network scope (EuroGuard). This way a WASM app is actually governed by the
// sovereign capability model, not by a test flag.
use crate::ring3::{CAP_CONSOLE, CAP_FILE, CAP_NET};
use eurosandbox::{Container, NetScope};

/// A WASI host bound to one container: every host call is checked against the
/// EFFECTIVE capabilities (`base ∩ container.caps`) and, for network, the net scope.
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
                        "network scope forbids {}.{}.{}.{}:{}",
                        ip[0], ip[1], ip[2], ip[3], port
                    )));
                }
                Ok(vec![1])
            }
            _ => Err(WasmError::HostError(alloc::string::String::from("unknown import"))),
        }
    }
}

/// Module that imports `euro.sock_connect(ip, port)` and exports `try_net()->i32`
/// which tries to reach 10.0.2.2:80 — gateway for the container test.
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
    let ip = (10i64 << 24) | (2 << 8) | 2; // 10.0.2.2 big-endian packed
    let mut body = vec![0u8]; // no locals
    body.push(0x41);
    body.extend(sleb(ip));
    body.push(0x41);
    body.extend(sleb(80)); // port 80
    body.extend_from_slice(&[0x10, 0x00, 0x0b]); // call 0 (sock_connect); end
    let mut code = vec![1u8];
    code.extend(uleb(body.len() as u32));
    code.extend(body);
    w.extend(section(10, code));
    w
}

/// H4 follow-up self-test: run the same WASM module in THREE EuroSandbox containers and
/// show that the WASI `sock_connect` is governed by the container capabilities + net scope:
/// allowed, denied-without-CAP_NET, denied-by-scope.
pub fn container_selftest() {
    let bytes = build_net_module();
    let module = match Module::parse(&bytes) {
        Ok(m) => m,
        Err(e) => {
            crate::serial_println!("[h4-ctr] parse failed: {:?}", e);
            return;
        }
    };
    let base = CAP_CONSOLE | CAP_NET | CAP_FILE; // process base mask
    let ok = Container::new("net-ok", base, NetScope::Allow(vec![([10, 0, 2, 2], 80)]));
    let no_net = Container::new("no-net", CAP_CONSOLE, NetScope::Any); // CAP_NET removed
    let scoped = Container::new("scoped", base, NetScope::Allow(vec![([1, 1, 1, 1], 443)]));
    for ctr in [&ok, &no_net, &scoped] {
        let mut inst = Instance::new(&module);
        let mut host = ContainerWasiHost { c: ctr, base };
        match inst.invoke("try_net", &[], &mut host) {
            Ok(v) => crate::serial_println!(
                "[h4-ctr] container '{}' (caps {:#06b}): sock_connect ALLOWED → {:?}",
                ctr.name,
                ctr.effective_caps(base),
                v
            ),
            Err(WasmError::CapabilityDenied(why)) => crate::serial_println!(
                "[h4-ctr] container '{}' (caps {:#06b}): sock_connect DENIED — {} ✓",
                ctr.name,
                ctr.effective_caps(base),
                why
            ),
            Err(e) => crate::serial_println!("[h4-ctr] container '{}': error {:?}", ctr.name, e),
        }
    }
}

// ── 3C-4: real WASI preview1 host + a hand-built wasm32-wasi test module ─────

/// A real `wasi_snapshot_preview1` host: implements the actual WASI ABI (iovec
/// arrays, `*nwritten` out-pointer, i32 errno return) for the core imports —
/// `fd_write`, `proc_exit`, `random_get`, `clock_time_get`,
/// `environ_sizes_get`, `args_sizes_get`. `fd_write` is gated on CAP_CONSOLE
/// (the EuroGuard sandbox boundary). Unlike the earlier `euro.fd_write` shim,
/// this follows the WASI ABI a `clang`/`rustc` `wasm32-wasi` binary emits.
struct WasiPreview1 {
    cap_console: bool,
    out: Vec<u8>,
    exited: Option<i32>,
}

fn rd_u32(mem: &[u8], p: usize) -> u32 {
    if p + 4 <= mem.len() {
        u32::from_le_bytes([mem[p], mem[p + 1], mem[p + 2], mem[p + 3]])
    } else {
        0
    }
}
fn wr_u32(mem: &mut [u8], p: usize, v: u32) {
    if p + 4 <= mem.len() {
        mem[p..p + 4].copy_from_slice(&v.to_le_bytes());
    }
}

const WASI_ESUCCESS: i64 = 0;
const WASI_EBADF: i64 = 8;

impl HostImports for WasiPreview1 {
    fn call(&mut self, m: &str, n: &str, args: &[Val], mem: &mut [u8]) -> Result<Vec<Val>, WasmError> {
        if m != "wasi_snapshot_preview1" {
            return Err(WasmError::HostError(String::from("unknown module")));
        }
        match n {
            // fd_write(fd, iovs_ptr, iovs_len, nwritten_ptr) -> errno
            "fd_write" => {
                if !self.cap_console {
                    return Err(WasmError::CapabilityDenied(String::from("CAP_CONSOLE for fd_write")));
                }
                let fd = args[0];
                let iovs = args[1] as usize;
                let iovs_len = args[2] as usize;
                let nwritten = args[3] as usize;
                if fd != 1 && fd != 2 {
                    return Ok(vec![WASI_EBADF]); // only stdout/stderr
                }
                let mut total = 0u32;
                // Each iovec is 8 bytes: {buf_ptr: u32, buf_len: u32}.
                for i in 0..iovs_len {
                    let base = iovs + i * 8;
                    let ptr = rd_u32(mem, base) as usize;
                    let len = rd_u32(mem, base + 4) as usize;
                    if ptr + len <= mem.len() {
                        self.out.extend_from_slice(&mem[ptr..ptr + len]);
                        total += len as u32;
                    }
                }
                wr_u32(mem, nwritten, total);
                Ok(vec![WASI_ESUCCESS])
            }
            // proc_exit(code) -> (noreturn)
            "proc_exit" => {
                self.exited = Some(args[0] as i32);
                Ok(vec![])
            }
            // random_get(buf, len) -> errno
            "random_get" => {
                let buf = args[0] as usize;
                let len = args[1] as usize;
                if buf + len <= mem.len() {
                    let mut tmp = vec![0u8; len];
                    crate::entropy::getrandom(&mut tmp);
                    mem[buf..buf + len].copy_from_slice(&tmp);
                }
                Ok(vec![WASI_ESUCCESS])
            }
            // clock_time_get(id, precision, time_ptr) -> errno
            "clock_time_get" => {
                let time_ptr = args[2] as usize;
                let ns = crate::rtc::epoch().saturating_mul(1_000_000_000);
                if time_ptr + 8 <= mem.len() {
                    mem[time_ptr..time_ptr + 8].copy_from_slice(&ns.to_le_bytes());
                }
                Ok(vec![WASI_ESUCCESS])
            }
            // environ_sizes_get / args_sizes_get(count_ptr, size_ptr) -> errno (empty env/args)
            "environ_sizes_get" | "args_sizes_get" => {
                wr_u32(mem, args[0] as usize, 0);
                wr_u32(mem, args[1] as usize, 0);
                Ok(vec![WASI_ESUCCESS])
            }
            _ => Err(WasmError::HostError(alloc::format!("unimplemented WASI import: {n}"))),
        }
    }
}

/// A real `wasm32-wasi`-shaped module: imports `wasi_snapshot_preview1.fd_write`
/// with the true `(i32,i32,i32,i32)->i32` signature, carries an **iovec + message
/// in a data section**, and `_start` calls `fd_write(1, iovs=0, iovs_len=1,
/// nwritten=8)`. This is exactly what a clang/rustc WASI "hello" emits (minus the
/// larger runtime), so running it proves the WASI ABI — not a custom shim.
fn build_wasi_module() -> Vec<u8> {
    let msg: &[u8] = b"Hello from a real WASI fd_write!\n";
    let mut w = vec![0u8, 0x61, 0x73, 0x6d, 1, 0, 0, 0];
    // types: 0 = (i32,i32,i32,i32)->i32, 1 = ()->()
    w.extend(section(1, vec![2, 0x60, 4, 0x7f, 0x7f, 0x7f, 0x7f, 1, 0x7f, 0x60, 0, 0]));
    // import wasi_snapshot_preview1.fd_write : type 0
    let mut im = vec![1u8, 22];
    im.extend_from_slice(b"wasi_snapshot_preview1");
    im.push(8);
    im.extend_from_slice(b"fd_write");
    im.extend_from_slice(&[0x00, 0]); // kind func, type 0
    w.extend(section(2, im));
    w.extend(section(3, vec![1, 1])); // 1 function, type 1
    w.extend(section(5, vec![1, 0x00, 1])); // 1 memory page
    let mut ex = vec![1u8, 6];
    ex.extend_from_slice(b"_start");
    ex.extend_from_slice(&[0x00, 1]); // export "_start" = func index 1
    w.extend(section(7, ex));
    // code for _start: fd_write(1, 0, 1, 8); drop; end
    let body = vec![
        0x00u8, // no locals
        0x41, 0x01, // i32.const 1 (fd = stdout)
        0x41, 0x00, // i32.const 0 (iovs_ptr)
        0x41, 0x01, // i32.const 1 (iovs_len)
        0x41, 0x08, // i32.const 8 (nwritten_ptr)
        0x10, 0x00, // call 0 (fd_write)
        0x1a, // drop (ignore errno)
        0x0b, // end
    ];
    let mut code = vec![1u8];
    code.extend(uleb(body.len() as u32));
    code.extend(body);
    w.extend(section(10, code));
    // data section: at offset 0 place iovec {ptr=16, len=msglen}, scratch, message@16.
    let mut seg = Vec::new();
    seg.extend_from_slice(&16u32.to_le_bytes()); // iovec.buf_ptr = 16
    seg.extend_from_slice(&(msg.len() as u32).to_le_bytes()); // iovec.buf_len
    seg.extend_from_slice(&[0u8; 8]); // bytes 8..16 = nwritten scratch + pad
    seg.extend_from_slice(msg); // message at offset 16
    let mut d = vec![1u8, 0u8]; // 1 active segment, mem 0
    d.extend_from_slice(&[0x41, 0x00, 0x0b]); // offset = i32.const 0, end
    d.extend(uleb(seg.len() as u32));
    d.extend(seg);
    w.extend(section(11, d));
    w
}

/// `[wasi]` boot self-test — run the real-WASI module against [`WasiPreview1`]:
/// with CAP_CONSOLE the `fd_write` iovec ABI writes the message and sets
/// `*nwritten`; without it, the sandbox denies the import.
pub fn wasi_selftest() {
    let bytes = build_wasi_module();
    let module = match Module::parse(&bytes) {
        Ok(m) => m,
        Err(e) => {
            crate::serial_println!("[wasi] WASI module parse failed: {:?}", e);
            return;
        }
    };
    // (1) With CAP_CONSOLE: the real WASI ABI writes the message.
    let mut inst = Instance::new(&module);
    let mut host = WasiPreview1 { cap_console: true, out: Vec::new(), exited: None };
    let ran = inst.invoke("_start", &[], &mut host).is_ok();
    let wrote = core::str::from_utf8(&host.out).unwrap_or("").contains("real WASI fd_write");
    // The module wrote *nwritten at offset 8 = message length.
    let nwritten_ok = {
        let m = inst.mem();
        m.len() >= 12 && u32::from_le_bytes([m[8], m[9], m[10], m[11]]) as usize == host.out.len()
    };

    // (2) Without the capability: the WASI import is denied (sandbox boundary).
    let mut inst2 = Instance::new(&module);
    let mut deny = WasiPreview1 { cap_console: false, out: Vec::new(), exited: None };
    let denied = matches!(inst2.invoke("_start", &[], &mut deny), Err(WasmError::CapabilityDenied(_)));

    let ok = ran && wrote && nwritten_ok && denied;
    crate::serial_println!(
        "[wasi] real WASI preview1 (wasi_snapshot_preview1.fd_write, true iovec ABI): ran={ran}, iovec-message-written={wrote}, *nwritten-set-correctly={nwritten_ok}, cap-gate-denies-without-CAP_CONSOLE={denied} → {}",
        if ok { "OK (runs real wasm32-wasi fd_write, not a custom shim; proc_exit/random_get/clock/env/args also implemented) ✓" } else { "FAILED ✗" }
    );
}
