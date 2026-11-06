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

// ── Sprint WP6: windowed WASM apps on the Euro API ──────────────────────────
//
// The second binding of the Euro API: the same calls as the native syscalls,
// as ONE import `euro.call(nr, a1..a5) -> i64` of the module. A WASM app is
// frame-driven: it exports `init() -> i32` (0 = ready, else an exit code) and
// `frame() -> i32` (0 = keep running, else done); the desktop loop calls
// `frame` once per tick with a step budget (fuel), so a module can never hold
// the desktop, and `event_wait`/`sleep_ms` return at once. Every pointer and
// length an import receives is checked against the module's linear memory;
// the surface, events and text go through `euroapi` under a pid of their own,
// so the hosted window, focus and input work exactly as for a native app.

/// Steps one `frame()`/`init()` call may execute before it traps (fuel).
pub const FRAME_FUEL: u64 = 4_000_000;

pub struct EuroHost<'a> {
    pub pid: u64,
    pub caps: u64,
    pub fs: Option<&'a mut dyn FileSystem>,
    /// Set by `exit(code)`; the trap that follows ends the call cleanly.
    pub exit: Option<i32>,
    pub output: Vec<String>,
}

fn span(mem: &[u8], ptr: u64, len: u64) -> Option<(usize, usize)> {
    let (p, n) = (ptr as usize, len as usize);
    if ptr > u32::MAX as u64 || len > u32::MAX as u64 || p.checked_add(n)? > mem.len() {
        None
    } else {
        Some((p, n))
    }
}

impl EuroHost<'_> {
    fn has(&self, cap: u64) -> bool {
        self.caps & cap == cap
    }

    fn dispatch(&mut self, nr: u64, a: [u64; 5], mem: &mut [u8]) -> i64 {
        use crate::ring3::{CAP_CONSOLE, CAP_FILE, CAP_WINDOW};
        const EPERM: i64 = -1;
        const EFAULT: i64 = -14;
        const EINVAL: i64 = -22;
        match nr {
            0x6100 => {
                let Some((p, n)) = span(mem, a[0], a[1].min(16)) else { return EFAULT };
                let (sw, sh) = crate::appgfx::screen();
                let mut b = [0u8; 16];
                for (i, v) in [crate::euroapi::ABI_VERSION, sw as u32, sh as u32, (self.caps & 0xffff_ffff) as u32].iter().enumerate() {
                    b[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
                }
                mem[p..p + n].copy_from_slice(&b[..n]);
                0
            }
            0x6101 => {
                if !self.has(CAP_WINDOW) { return EPERM; }
                crate::euroapi::create(self.pid, a[0] as usize, a[1] as usize)
            }
            0x6102 => {
                if !self.has(CAP_WINDOW) { return EPERM; }
                let (w, h) = (a[1] as usize, a[2] as usize);
                if w == 0 || h == 0 || w > crate::euroapi::MAX_W || h > crate::euroapi::MAX_H { return EINVAL; }
                let Some((p, n)) = span(mem, a[0], (w * h * 4) as u64) else { return EFAULT };
                let px: Vec<u32> = mem[p..p + n].chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
                crate::euroapi::present(self.pid, &px, w, h)
            }
            0x6103 => {
                if !self.has(CAP_WINDOW) { return EPERM; }
                match crate::euroapi::poll(self.pid) {
                    Some(ev) => {
                        let Some((p, n)) = span(mem, a[0], a[1].min(16)) else { return EFAULT };
                        mem[p..p + n].copy_from_slice(&ev.to_bytes()[..n]);
                        1
                    }
                    None => 0,
                }
            }
            0x6104 => {
                // Frame-driven: never block; the host calls frame() again next tick.
                if !self.has(CAP_WINDOW) { return EPERM; }
                crate::euroapi::pending(self.pid) as i64
            }
            0x6105 => {
                if !self.has(CAP_WINDOW) { return EPERM; }
                let (w, h) = ((a[1] >> 32) as usize, (a[1] & 0xffff_ffff) as usize);
                let (x, y) = ((a[2] >> 32) as u32 as i32, (a[2] & 0xffff_ffff) as u32 as i32);
                let len = a[4] >> 40;
                let size = ((a[4] >> 32) & 0xff) as u32;
                let argb = (a[4] & 0xff_ffff) as u32;
                if w == 0 || h == 0 || w > crate::euroapi::MAX_W || h > crate::euroapi::MAX_H || len > crate::euroapi::MAX_TEXT as u64 { return EINVAL; }
                let Some((p, n)) = span(mem, a[0], (w * h * 4) as u64) else { return EFAULT };
                let Some((sp, sn)) = span(mem, a[3], len) else { return EFAULT };
                let s = String::from_utf8_lossy(&mem[sp..sp + sn]).into_owned();
                let mut px: Vec<u32> = mem[p..p + n].chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
                let r = crate::euroapi::text_draw(&mut px, w, h, x, y, &s, argb, size);
                for (i, v) in px.iter().enumerate() {
                    mem[p + i * 4..p + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
                }
                r
            }
            0x6106 => {
                if a[1] > crate::euroapi::MAX_TEXT as u64 { return EINVAL; }
                let Some((sp, sn)) = span(mem, a[0], a[1]) else { return EFAULT };
                crate::euroapi::text_measure(&String::from_utf8_lossy(&mem[sp..sp + sn]), a[2] as u32) as i64
            }
            0x6107 => {
                if !self.has(CAP_CONSOLE) { return EPERM; }
                if a[1] > crate::euroapi::MAX_STR as u64 { return EINVAL; }
                let Some((sp, sn)) = span(mem, a[0], a[1]) else { return EFAULT };
                let line = String::from_utf8_lossy(&mem[sp..sp + sn]).into_owned();
                crate::serial_println!("[euroapp wasm pid {}] {}", self.pid, line.trim_end());
                if self.output.len() < 512 { self.output.push(line); }
                0
            }
            0x6108 => (crate::interrupts::ticks() * 10) as i64,
            0x6109 => 0, // sleep_ms: frame-driven, returns at once
            0x610A | 0x610B => {
                if !self.has(CAP_FILE) { return EPERM; }
                if a[1] > 512 { return EINVAL; }
                let Some((pp, pn)) = span(mem, a[0], a[1]) else { return EFAULT };
                let Some(path) = crate::euroapi::file_path_ok(&String::from_utf8_lossy(&mem[pp..pp + pn])) else { return -13 };
                let n = a[3].min(crate::euroapi::MAX_FILE as u64);
                let Some((bp, bn)) = span(mem, a[2], n) else { return EFAULT };
                let Some(fs) = self.fs.as_deref_mut() else { return -5 };
                if crate::euroapi::path_has_symlink(&*fs, &path) {
                    return -13; // a symlink could leave the scope: refused
                }
                if nr == 0x610A {
                    match fs.read_file(&path) {
                        Ok(data) => { let k = data.len().min(bn); mem[bp..bp + k].copy_from_slice(&data[..k]); k as i64 }
                        Err(_) => -2,
                    }
                } else {
                    if bn > crate::euroapi::MAX_FILE_WRITE || !crate::euroapi::write_budget(self.pid, bn) { return -28; }
                    let data = mem[bp..bp + bn].to_vec();
                    if fs.write_file(&path, &data).is_ok() { bn as i64 } else { -13 }
                }
            }
            0x610C => { self.exit = Some(a[0] as i32); 0 }
            _ => -38, // ENOSYS: not a Euro call
        }
    }
}

impl HostImports for EuroHost<'_> {
    fn call(&mut self, m: &str, n: &str, args: &[Val], mem: &mut [u8]) -> Result<Vec<Val>, WasmError> {
        if m != "euro" || n != "call" || args.len() != 6 {
            return Err(WasmError::Unsupported("import (only euro.call)"));
        }
        let a = [args[1] as u64, args[2] as u64, args[3] as u64, args[4] as u64, args[5] as u64];
        let r = self.dispatch(args[0] as u64, a, mem);
        if self.exit.is_some() {
            return Err(WasmError::Trap("exit"));
        }
        Ok(vec![r])
    }
}

/// The one windowed WASM app the desktop hosts (like the one native surface).
struct WasmApp {
    pid: u64,
    caps: u64,
    module: &'static Module,
    inst: Instance<'static>,
    frames: u64,
}

static APP: spin::Mutex<Option<WasmApp>> = spin::Mutex::new(None);

fn free_module(m: &'static Module) {
    // SAFETY: `m` came from Box::leak in start_app and no Instance refers to it
    // any more (the WasmApp holding the Instance was dropped first).
    unsafe { drop(alloc::boxed::Box::from_raw(m as *const Module as *mut Module)) };
}

/// Start a windowed WASM app: parse, instantiate, run `init()` (first frame).
/// Ok(()) = running (its surface exists); Err(text) = refused or ended in init.
pub fn start_app(bytes: &[u8], pid: u64, caps: u64, fs: &mut dyn FileSystem) -> Result<(), String> {
    if APP.lock().is_some() {
        return Err(String::from("a WASM app is already running (close its window first)"));
    }
    let module = Module::parse(bytes).map_err(|e| format!("not a runnable module: {e:?}"))?;
    if !module.has_export("init") || !module.has_export("frame") {
        return Err(String::from("a windowed WASM app must export init() and frame()"));
    }
    let module: &'static Module = alloc::boxed::Box::leak(alloc::boxed::Box::new(module));
    let mut inst = Instance::new(module);
    inst.set_fuel(FRAME_FUEL);
    let mut host = EuroHost { pid, caps, fs: Some(fs), exit: None, output: Vec::new() };
    let r = inst.invoke("init", &[], &mut host);
    let ended = match (&r, host.exit) {
        (_, Some(code)) => Some(format!("exit {code}")),
        (Ok(v), None) if v.first().copied().unwrap_or(0) != 0 => Some(format!("init returned {}", v[0])),
        (Err(e), None) => Some(format!("trap in init: {e:?}")),
        _ => None,
    };
    if let Some(why) = ended {
        drop(inst);
        crate::euroapi::remove(pid);
        free_module(module);
        return Err(why);
    }
    *APP.lock() = Some(WasmApp { pid, caps, module, inst, frames: 1 });
    Ok(())
}

/// One desktop tick: run `frame()` under the fuel budget. `None` = still
/// running; `Some(Ok(code))` = ended by itself; `Some(Err(why))` = trapped.
pub fn pump(fs: &mut dyn FileSystem) -> Option<Result<i32, String>> {
    let mut g = APP.lock();
    let app = g.as_mut()?;
    let mut host = EuroHost { pid: app.pid, caps: app.caps, fs: Some(fs), exit: None, output: Vec::new() };
    let r = app.inst.invoke("frame", &[], &mut host);
    app.frames += 1;
    let done = match (r, host.exit) {
        (_, Some(code)) => Some(Ok(code)),
        (Ok(v), None) => if v.first().copied().unwrap_or(0) != 0 { Some(Ok(0)) } else { None },
        (Err(e), None) => Some(Err(format!("{e:?}"))),
    };
    if done.is_some() {
        let app = g.take().unwrap();
        let (pid, module) = (app.pid, app.module);
        drop(app);
        crate::euroapi::remove(pid);
        free_module(module);
    }
    done
}

pub fn app_alive(pid: u64) -> bool {
    APP.lock().as_ref().map(|a| a.pid == pid).unwrap_or(false)
}

/// Terminate the hosted WASM app (window closed and it did not exit on Close).
pub fn kill(pid: u64) {
    let mut g = APP.lock();
    if g.as_ref().map(|a| a.pid == pid).unwrap_or(false) {
        let app = g.take().unwrap();
        let module = app.module;
        drop(app);
        crate::euroapi::remove(pid);
        free_module(module);
    }
}

/// A console WASM app (`display = none`): `init()` does the work; `frame()` is
/// pumped up to 50 times for an app that needs it (a bounded total budget: the
/// shell waits for this call). Returns (output, exit).
pub fn run_cli(bytes: &[u8], pid: u64, caps: u64, fs: &mut dyn FileSystem) -> (Vec<String>, i32) {
    let module = match Module::parse(bytes) {
        Ok(m) => m,
        Err(e) => return (vec![format!("wasm: not a runnable module: {e:?}")], -1),
    };
    let mut inst = Instance::new(&module);
    inst.set_fuel(FRAME_FUEL * 25);
    let mut host = EuroHost { pid, caps, fs: Some(fs), exit: None, output: Vec::new() };
    let mut code = match inst.invoke("init", &[], &mut host) {
        Ok(v) => v.first().copied().unwrap_or(0) as i32,
        Err(e) if host.exit.is_none() => { host.output.push(format!("wasm: trap: {e:?}")); -1 }
        Err(_) => 0,
    };
    if let Some(c) = host.exit { code = c; }
    if host.exit.is_none() && code == 0 && module.has_export("frame") {
        for _ in 0..50 {
            match inst.invoke("frame", &[], &mut host) {
                Ok(v) if v.first().copied().unwrap_or(0) == 0 && host.exit.is_none() => continue,
                Ok(_) => break,
                Err(e) if host.exit.is_none() => { host.output.push(format!("wasm: trap: {e:?}")); code = -1; break; }
                Err(_) => break,
            }
        }
        if let Some(c) = host.exit { code = c; }
    }
    crate::euroapi::remove(pid);
    (host.output, code)
}

/// `[euro-fscope]` self-test: the WASM host's file scope. A symlink the user
/// placed in their home pointing outside it is refused; a plain file in the
/// home round-trips.
pub fn fscope_selftest(fs: &mut dyn FileSystem) {
    use eurofs::FileSystem as _;
    let _ = fs.create_dir("/home");
    let _ = fs.create_dir("/home/euro");
    let _ = fs.remove_file("/home/euro/escape-link");
    let link_made = fs.create_symlink("/home/euro/escape-link", "/etc/euroguard/system.conf").is_ok();
    let mut host = EuroHost { pid: 777, caps: crate::ring3::CAP_FILE, fs: Some(fs), exit: None, output: Vec::new() };
    let mut mem = vec![0u8; 65536];
    let p = b"escape-link";
    mem[..p.len()].copy_from_slice(p);
    let via_link = host.dispatch(0x610A, [0, p.len() as u64, 4096, 1024, 0], &mut mem);
    let q = b"scope-test.txt";
    mem[..q.len()].copy_from_slice(q);
    mem[8192..8192 + 5].copy_from_slice(b"hello");
    let wrote = host.dispatch(0x610B, [0, q.len() as u64, 8192, 5, 0], &mut mem);
    let read = host.dispatch(0x610A, [0, q.len() as u64, 4096, 1024, 0], &mut mem);
    let roundtrip = wrote == 5 && read == 5 && &mem[4096..4101] == b"hello";
    let outside = host.dispatch(0x610A, [0, 0, 4096, 1024, 0], &mut mem); // empty path
    drop(host);
    let _ = fs.remove_file("/home/euro/escape-link");
    let _ = fs.remove_file("/home/euro/scope-test.txt");
    let ok = link_made && via_link == -13 && roundtrip && outside == -13;
    crate::serial_println!(
        "[euro-fscope] WASM file scope: symlink-in-home-refused={} · plain-file-roundtrip={roundtrip} · empty-path-refused={} → {}",
        via_link == -13, outside == -13,
        if ok { "OK (home only, no symlink escape) ✓" } else { "FAILED" }
    );
}

/// A module whose `run` loops forever: the fuel bound must end it (self-test).
pub fn build_spin_module() -> Vec<u8> {
    let mut w = vec![0u8, 0x61, 0x73, 0x6d, 1, 0, 0, 0];
    w.extend(section(1, vec![1, 0x60, 0, 1, 0x7f])); // type 0: () -> i32
    w.extend(section(3, vec![1, 0])); // 1 function of type 0
    let mut ex = vec![1u8, 3];
    ex.extend_from_slice(b"run");
    ex.extend_from_slice(&[0x00, 0]);
    w.extend(section(7, ex));
    let body = vec![0u8, 0x03, 0x40, 0x0c, 0x00, 0x0b, 0x41, 0x07, 0x0b]; // loop { br 0 } ; 7
    let mut code = vec![1u8];
    code.extend(uleb(body.len() as u32));
    code.extend(body);
    w.extend(section(10, code));
    w
}

/// Does the fuel bound end a spinning module, and does a small budget end
/// it fast? (Host-tested too; here against the kernel build of eurowasm.)
pub fn fuel_bounded() -> bool {
    let bytes = build_spin_module();
    let Ok(m) = Module::parse(&bytes) else { return false };
    let mut inst = Instance::new(&m);
    inst.set_fuel(10_000);
    matches!(inst.invoke("run", &[], &mut eurowasm::NoImports), Err(WasmError::Trap(_)))
}
