//! EuroWASM — een minimale **no-JIT WebAssembly-interpreter** (plan H4).
//!
//! Een sandbox-vriendelijk, architectuur-onafhankelijk app-formaat: WASM-modules
//! draaien geïnterpreteerd (geen native code-generatie), en hun **imports** worden
//! op **EuroGuard-capabilities** afgebeeld — een host-functie als `fd_write` mag
//! alleen draaien als het proces de bijbehorende capability (`CAP_FILE`/`CAP_NET`/
//! `CAP_CONSOLE`) bezit. Zo is "ongesigneerde derde-partij-code draaien" veilig.
//!
//! Ondersteund: i32/i64-rekenkunde + vergelijkingen, lokale variabelen, gestructu-
//! reerde control-flow (`block`/`loop`/`if`/`else`/`br`/`br_if`/`return`), `call`
//! (incl. recursie) + geïmporteerde host-calls, lineair geheugen met `i32.load`/
//! `i32.store`/`memory.grow`, `drop`/`select`/`global.get`/`global.set`. `no_std`
//! + alloc; de parser én de interpreter zijn volledig op de host getest.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// Een WASM-waarde. We bewaren alles in een i64; i32-operaties maskeren naar 32 bits.
pub type Val = i64;

/// Fouten van parser of interpreter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WasmError {
    BadMagic,
    Truncated,
    Unsupported(&'static str),
    NoSuchExport,
    Trap(&'static str),
    /// Een host-import vereiste een capability die het proces niet heeft.
    CapabilityDenied(String),
    HostError(String),
}

// ── LEB128 + byte-reader ───────────────────────────────────────────────────
struct Reader<'a> {
    d: &'a [u8],
    p: usize,
}
impl<'a> Reader<'a> {
    fn new(d: &'a [u8]) -> Self {
        Reader { d, p: 0 }
    }
    fn eof(&self) -> bool {
        self.p >= self.d.len()
    }
    fn byte(&mut self) -> Result<u8, WasmError> {
        let b = *self.d.get(self.p).ok_or(WasmError::Truncated)?;
        self.p += 1;
        Ok(b)
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], WasmError> {
        if self.p + n > self.d.len() {
            return Err(WasmError::Truncated);
        }
        let s = &self.d[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn uleb(&mut self) -> Result<u64, WasmError> {
        let (mut r, mut sh) = (0u64, 0u32);
        loop {
            let b = self.byte()?;
            r |= ((b & 0x7f) as u64) << sh;
            if b & 0x80 == 0 {
                return Ok(r);
            }
            sh += 7;
            if sh >= 64 {
                return Err(WasmError::Truncated);
            }
        }
    }
    fn sleb(&mut self) -> Result<i64, WasmError> {
        let (mut r, mut sh) = (0i64, 0u32);
        loop {
            let b = self.byte()?;
            r |= ((b & 0x7f) as i64) << sh;
            sh += 7;
            if b & 0x80 == 0 {
                if sh < 64 && (b & 0x40) != 0 {
                    r |= -1i64 << sh; // sign-extend
                }
                return Ok(r);
            }
            if sh >= 64 {
                return Err(WasmError::Truncated);
            }
        }
    }
    fn name(&mut self) -> Result<String, WasmError> {
        let n = self.uleb()? as usize;
        let b = self.bytes(n)?;
        Ok(String::from_utf8_lossy(b).into_owned())
    }
}

// ── Gedecodeerde instructie ────────────────────────────────────────────────
#[derive(Debug, Clone)]
enum Op {
    Unreachable,
    Nop,
    Block { end: u32, arity: u8 },
    Loop { arity: u8 },
    If { else_: u32, end: u32, arity: u8 },
    Else { end: u32 },
    End,
    Br(u32),
    BrIf(u32),
    Return,
    Call(u32),
    Drop,
    Select,
    LocalGet(u32),
    LocalSet(u32),
    LocalTee(u32),
    GlobalGet(u32),
    GlobalSet(u32),
    I32Const(i32),
    I64Const(i64),
    F32Const(u32),
    F64Const(u64),
    I32Load(u32),
    I32Store(u32),
    MemoryGrow,
    MemorySize,
    Num(u8), // numerieke opcode, gedispatcht in exec
}

fn blocktype_arity(b: u8) -> u8 {
    // 0x40 = leeg, 0x7f/0x7e/0x7d/0x7c = één resultaat. (Type-index-blocktypes:
    // minimaal niet ondersteund → behandeld als arity 0.)
    if b == 0x40 {
        0
    } else {
        1
    }
}

#[derive(Debug, Clone)]
struct Func {
    type_idx: u32,
    n_params: u32,
    n_results: u32,
    n_locals: u32, // gedeclareerde locals (boven de params)
    code: Vec<Op>,
}

#[derive(Debug, Clone)]
struct ImportFn {
    module: String,
    name: String,
    type_idx: u32,
    n_params: u32,
    n_results: u32,
}

/// Een geparste WASM-module.
#[derive(Debug, Clone)]
pub struct Module {
    types: Vec<(Vec<u8>, Vec<u8>)>, // (params, results) als valtype-bytes
    imports: Vec<ImportFn>,
    funcs: Vec<Func>, // de IN deze module gedefinieerde functies
    exports: Vec<(String, u32)>, // naam → globale functie-index (imports eerst)
    mem_min_pages: u32,
    globals: Vec<i64>,
}

impl Module {
    /// Parse een WASM-binary (`\0asm` + secties).
    pub fn parse(bytes: &[u8]) -> Result<Module, WasmError> {
        let mut r = Reader::new(bytes);
        if r.bytes(4)? != b"\0asm" {
            return Err(WasmError::BadMagic);
        }
        if r.bytes(4)? != [1, 0, 0, 0] {
            return Err(WasmError::Unsupported("wasm-versie"));
        }
        let mut m = Module {
            types: Vec::new(),
            imports: Vec::new(),
            funcs: Vec::new(),
            exports: Vec::new(),
            mem_min_pages: 0,
            globals: Vec::new(),
        };
        let mut func_type_idx: Vec<u32> = Vec::new(); // type-index per gedefinieerde functie

        while !r.eof() {
            let id = r.byte()?;
            let size = r.uleb()? as usize;
            let body = r.bytes(size)?;
            let mut s = Reader::new(body);
            match id {
                1 => {
                    // Type-sectie.
                    let n = s.uleb()?;
                    for _ in 0..n {
                        if s.byte()? != 0x60 {
                            return Err(WasmError::Unsupported("functype-vorm"));
                        }
                        let np = s.uleb()? as usize;
                        let params = s.bytes(np)?.to_vec();
                        let nr = s.uleb()? as usize;
                        let results = s.bytes(nr)?.to_vec();
                        m.types.push((params, results));
                    }
                }
                2 => {
                    // Import-sectie (alleen functie-imports → host-calls).
                    let n = s.uleb()?;
                    for _ in 0..n {
                        let module = s.name()?;
                        let name = s.name()?;
                        let kind = s.byte()?;
                        match kind {
                            0x00 => {
                                let ti = s.uleb()? as u32;
                                let (p, rr) = m.types.get(ti as usize).cloned().unwrap_or_default();
                                m.imports.push(ImportFn {
                                    module,
                                    name,
                                    type_idx: ti,
                                    n_params: p.len() as u32,
                                    n_results: rr.len() as u32,
                                });
                            }
                            0x01 => {
                                let _ = s.byte()?; // table elemtype
                                read_limits(&mut s)?;
                            }
                            0x02 => {
                                read_limits(&mut s)?;
                            }
                            0x03 => {
                                let _ = s.byte()?; // global valtype
                                let _ = s.byte()?; // mut
                            }
                            _ => return Err(WasmError::Unsupported("import-soort")),
                        }
                    }
                }
                3 => {
                    let n = s.uleb()?;
                    for _ in 0..n {
                        func_type_idx.push(s.uleb()? as u32);
                    }
                }
                5 => {
                    let n = s.uleb()?;
                    for _ in 0..n {
                        let (min, _max) = read_limits(&mut s)?;
                        m.mem_min_pages = min;
                    }
                }
                6 => {
                    let n = s.uleb()?;
                    for _ in 0..n {
                        let _vt = s.byte()?;
                        let _mut = s.byte()?;
                        // init-expr: const + end
                        let v = read_const_expr(&mut s)?;
                        m.globals.push(v);
                    }
                }
                7 => {
                    let n = s.uleb()?;
                    for _ in 0..n {
                        let name = s.name()?;
                        let kind = s.byte()?;
                        let idx = s.uleb()? as u32;
                        if kind == 0x00 {
                            m.exports.push((name, idx));
                        }
                    }
                }
                10 => {
                    let n = s.uleb()? as usize;
                    for fi in 0..n {
                        let body_sz = s.uleb()? as usize;
                        let body = s.bytes(body_sz)?;
                        let mut c = Reader::new(body);
                        // Locals: vec van (count, valtype).
                        let nl = c.uleb()?;
                        let mut n_locals = 0u32;
                        for _ in 0..nl {
                            let cnt = c.uleb()? as u32;
                            let _vt = c.byte()?;
                            n_locals += cnt;
                        }
                        let ti = *func_type_idx.get(fi).ok_or(WasmError::Truncated)?;
                        let (params, results) = m.types.get(ti as usize).cloned().unwrap_or_default();
                        let code = decode_body(&mut c)?;
                        m.funcs.push(Func {
                            type_idx: ti,
                            n_params: params.len() as u32,
                            n_results: results.len() as u32,
                            n_locals,
                            code,
                        });
                    }
                }
                _ => { /* andere secties (custom/data/element/start) overslaan */ }
            }
        }
        Ok(m)
    }

    fn n_imports(&self) -> u32 {
        self.imports.len() as u32
    }
}

fn read_limits(s: &mut Reader) -> Result<(u32, u32), WasmError> {
    let flag = s.byte()?;
    let min = s.uleb()? as u32;
    let max = if flag & 1 != 0 { s.uleb()? as u32 } else { 0 };
    Ok((min, max))
}

fn read_const_expr(s: &mut Reader) -> Result<i64, WasmError> {
    let op = s.byte()?;
    let v = match op {
        0x41 => s.sleb()?,            // i32.const
        0x42 => s.sleb()?,            // i64.const
        _ => return Err(WasmError::Unsupported("const-expr")),
    };
    if s.byte()? != 0x0b {
        return Err(WasmError::Unsupported("const-expr-end"));
    }
    Ok(v)
}

/// Decodeer een functie-body tot ops met OPGELOSTE control-flow-targets.
fn decode_body(c: &mut Reader) -> Result<Vec<Op>, WasmError> {
    let mut ops: Vec<Op> = Vec::new();
    let mut ctrl: Vec<usize> = Vec::new(); // indices van open block/loop/if
    loop {
        if c.eof() {
            break;
        }
        let op = c.byte()?;
        match op {
            0x00 => ops.push(Op::Unreachable),
            0x01 => ops.push(Op::Nop),
            0x02 => {
                let bt = c.byte()?;
                ctrl.push(ops.len());
                ops.push(Op::Block { end: 0, arity: blocktype_arity(bt) });
            }
            0x03 => {
                let bt = c.byte()?;
                ctrl.push(ops.len());
                ops.push(Op::Loop { arity: blocktype_arity(bt) });
            }
            0x04 => {
                let bt = c.byte()?;
                ctrl.push(ops.len());
                ops.push(Op::If { else_: 0, end: 0, arity: blocktype_arity(bt) });
            }
            0x05 => {
                // else: koppel aan de open if.
                let i = *ctrl.last().ok_or(WasmError::Truncated)?;
                let here = ops.len() as u32;
                if let Op::If { else_, .. } = &mut ops[i] {
                    *else_ = here;
                }
                ops.push(Op::Else { end: 0 });
            }
            0x0b => {
                // end: sluit de bovenste open control (of de functie zelf).
                if let Some(i) = ctrl.pop() {
                    let here = ops.len() as u32;
                    let mut fix_else = None;
                    match &mut ops[i] {
                        Op::Block { end, .. } => *end = here,
                        Op::If { else_, end, .. } => {
                            if *else_ == 0 {
                                *else_ = here; // geen else → spring naar end
                            }
                            *end = here;
                            fix_else = Some(*else_);
                        }
                        Op::Loop { .. } => {}
                        _ => {}
                    }
                    // Vul de Else's end-target in (aparte borrow, na ops[i]).
                    if let Some(ei) = fix_else {
                        if let Some(Op::Else { end: ee }) = ops.get_mut(ei as usize) {
                            *ee = here;
                        }
                    }
                }
                ops.push(Op::End);
            }
            0x0c => ops.push(Op::Br(c.uleb()? as u32)),
            0x0d => ops.push(Op::BrIf(c.uleb()? as u32)),
            0x0f => ops.push(Op::Return),
            0x10 => ops.push(Op::Call(c.uleb()? as u32)),
            0x1a => ops.push(Op::Drop),
            0x1b => ops.push(Op::Select),
            0x20 => ops.push(Op::LocalGet(c.uleb()? as u32)),
            0x21 => ops.push(Op::LocalSet(c.uleb()? as u32)),
            0x22 => ops.push(Op::LocalTee(c.uleb()? as u32)),
            0x23 => ops.push(Op::GlobalGet(c.uleb()? as u32)),
            0x24 => ops.push(Op::GlobalSet(c.uleb()? as u32)),
            0x28 => {
                let _align = c.uleb()?;
                ops.push(Op::I32Load(c.uleb()? as u32));
            }
            0x36 => {
                let _align = c.uleb()?;
                ops.push(Op::I32Store(c.uleb()? as u32));
            }
            0x3f => {
                let _ = c.byte()?; // mem-index 0
                ops.push(Op::MemorySize);
            }
            0x40 => {
                let _ = c.byte()?;
                ops.push(Op::MemoryGrow);
            }
            0x41 => ops.push(Op::I32Const(c.sleb()? as i32)),
            0x42 => ops.push(Op::I64Const(c.sleb()?)),
            0x43 => {
                let b = c.bytes(4)?;
                ops.push(Op::F32Const(u32::from_le_bytes([b[0], b[1], b[2], b[3]])));
            }
            0x44 => {
                let b = c.bytes(8)?;
                ops.push(Op::F64Const(u64::from_le_bytes([
                    b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
                ])));
            }
            // Numerieke ops: i32/i64 (rekenkunde/vergelijkingen) + f32/f64 (rekenkunde/
            // vergelijkingen) + alle conversies/reinterpretaties.
            0x45..=0x78 | 0x50..=0x5a | 0x5b..=0x66 | 0x7c..=0x8a | 0x8b..=0xa6 | 0xa7..=0xc4 => {
                ops.push(Op::Num(op))
            }
            _ => return Err(WasmError::Unsupported("opcode")),
        }
    }
    Ok(ops)
}

/// Een host-import: roept een EuroGuard-bewaakte host-functie aan. Geeft de
/// resultaten terug, of een fout (bv. capability geweigerd).
pub trait HostImports {
    /// `module`/`name` = de import-identiteit (bv. "euro"/"fd_write"). `args` =
    /// de WASM-stackargumenten. `mem` = lineair geheugen (voor pointers/lengtes).
    fn call(
        &mut self,
        module: &str,
        name: &str,
        args: &[Val],
        mem: &mut [u8],
    ) -> Result<Vec<Val>, WasmError>;
}

/// Een host die geen imports aanbiedt (voor pure-rekenmodules).
pub struct NoImports;
impl HostImports for NoImports {
    fn call(&mut self, m: &str, n: &str, _: &[Val], _: &mut [u8]) -> Result<Vec<Val>, WasmError> {
        Err(WasmError::HostError(alloc::format!("onbekende import {m}.{n}")))
    }
}

const PAGE: usize = 65536;
/// Harde bovengrens op het lineair geheugen van een module (256 pagina's = 16 MiB):
/// een onvertrouwde WASM-agent mag de kernel-allocator niet kunnen uitputten (audit H6).
const MAX_MEM_PAGES: usize = 256;

struct Frame {
    code_idx: usize, // index in module.funcs
    ip: usize,
    locals: Vec<i64>,
    ctrl: Vec<CtrlEntry>,
    sp_base: usize, // value-stack-hoogte bij frame-start
}

#[derive(Clone, Copy)]
struct CtrlEntry {
    target: u32,
    height: usize,
    arity: u8,
    is_loop: bool,
}

/// Een instantie: een module + zijn lineair geheugen + globals, klaar om
/// geëxporteerde functies aan te roepen.
pub struct Instance<'m> {
    m: &'m Module,
    mem: Vec<u8>,
    globals: Vec<i64>,
}

impl<'m> Instance<'m> {
    pub fn new(m: &'m Module) -> Self {
        Instance {
            m,
            mem: vec![0u8; m.mem_min_pages as usize * PAGE],
            globals: m.globals.clone(),
        }
    }

    /// Schrijf bytes in het lineair geheugen (om argumenten voor te bereiden).
    pub fn write_mem(&mut self, off: usize, data: &[u8]) -> Result<(), WasmError> {
        if off + data.len() > self.mem.len() {
            return Err(WasmError::Trap("mem-write out of bounds"));
        }
        self.mem[off..off + data.len()].copy_from_slice(data);
        Ok(())
    }

    pub fn mem(&self) -> &[u8] {
        &self.mem
    }

    /// Roep een geëxporteerde functie aan met `args`. Geeft de resultaten.
    pub fn invoke(
        &mut self,
        export: &str,
        args: &[Val],
        host: &mut dyn HostImports,
    ) -> Result<Vec<Val>, WasmError> {
        let gidx = self
            .m
            .exports
            .iter()
            .find(|(n, _)| n == export)
            .map(|(_, i)| *i)
            .ok_or(WasmError::NoSuchExport)?;
        self.call_global(gidx, args, host)
    }

    fn call_global(
        &mut self,
        gidx: u32,
        args: &[Val],
        host: &mut dyn HostImports,
    ) -> Result<Vec<Val>, WasmError> {
        let n_imp = self.m.n_imports();
        if gidx < n_imp {
            // Directe host-import-call.
            let imp = &self.m.imports[gidx as usize];
            return host.call(&imp.module, &imp.name, args, &mut self.mem);
        }
        let code_idx = (gidx - n_imp) as usize;
        self.run(code_idx, args, host)
    }

    fn run(
        &mut self,
        code_idx: usize,
        args: &[Val],
        host: &mut dyn HostImports,
    ) -> Result<Vec<Val>, WasmError> {
        let mut stack: Vec<i64> = Vec::new();
        let mut frames: Vec<Frame> = Vec::new();
        frames.push(self.make_frame(code_idx, args, 0)?);

        let mut steps: u64 = 0;
        'frames: loop {
            // Werk met de bovenste frame.
            let fi = frames.len() - 1;
            loop {
                steps += 1;
                if steps > 50_000_000 {
                    return Err(WasmError::Trap("step-limiet (mogelijke oneindige lus)"));
                }
                let f = &mut frames[fi];
                let func = &self.m.funcs[f.code_idx];
                if f.ip >= func.code.len() {
                    break; // impliciete functie-return
                }
                let op = func.code[f.ip].clone();
                f.ip += 1;
                match op {
                    Op::Unreachable => return Err(WasmError::Trap("unreachable")),
                    Op::Nop => {}
                    Op::Block { end, arity } => f.ctrl.push(CtrlEntry {
                        target: end + 1,
                        height: stack.len(),
                        arity,
                        is_loop: false,
                    }),
                    Op::Loop { arity } => f.ctrl.push(CtrlEntry {
                        target: f.ip as u32, // loop-body-start
                        height: stack.len(),
                        arity,
                        is_loop: true,
                    }),
                    Op::If { else_, end, arity } => {
                        let cond = stack.pop().ok_or(WasmError::Trap("stack"))?;
                        f.ctrl.push(CtrlEntry {
                            target: end + 1,
                            height: stack.len(),
                            arity,
                            is_loop: false,
                        });
                        if cond == 0 {
                            f.ip = if else_ == end { end as usize } else { (else_ + 1) as usize };
                        }
                    }
                    Op::Else { end } => {
                        // Then-tak klaar → spring over de else-tak; pop de control.
                        f.ctrl.pop();
                        f.ip = (end + 1) as usize;
                    }
                    Op::End => {
                        f.ctrl.pop();
                    }
                    Op::Br(d) => do_branch(f, &mut stack, d)?,
                    Op::BrIf(d) => {
                        let cond = stack.pop().ok_or(WasmError::Trap("stack"))?;
                        if cond != 0 {
                            do_branch(f, &mut stack, d)?;
                        }
                    }
                    Op::Return => {
                        f.ip = func.code.len();
                    }
                    Op::Call(g) => {
                        let n_imp = self.m.n_imports();
                        let (np, nr) = self.fn_arity(g);
                        if stack.len() < np {
                            return Err(WasmError::Trap("call: te weinig args"));
                        }
                        let at = stack.len() - np;
                        let cargs: Vec<i64> = stack.split_off(at);
                        if g < n_imp {
                            let imp = &self.m.imports[g as usize];
                            let res = host.call(&imp.module, &imp.name, &cargs, &mut self.mem)?;
                            stack.extend(res);
                        } else {
                            // Push een nieuwe frame; verlaat de inner-lus.
                            let ci = (g - n_imp) as usize;
                            let nf = self.make_frame(ci, &cargs, stack.len())?;
                            frames.push(nf);
                            let _ = nr;
                            continue 'frames;
                        }
                    }
                    Op::Drop => {
                        stack.pop();
                    }
                    Op::Select => {
                        let c = stack.pop().ok_or(WasmError::Trap("stack"))?;
                        let b = stack.pop().ok_or(WasmError::Trap("stack"))?;
                        let a = stack.pop().ok_or(WasmError::Trap("stack"))?;
                        stack.push(if c != 0 { a } else { b });
                    }
                    // Lokale-index begrensd (audit H5): een gemaakte module met een
                    // out-of-range index mag de interpreter niet laten panieken.
                    Op::LocalGet(i) => {
                        let v = *f.locals.get(i as usize).ok_or(WasmError::Trap("local index"))?;
                        stack.push(v);
                    }
                    Op::LocalSet(i) => {
                        let v = stack.pop().ok_or(WasmError::Trap("stack"))?;
                        *f.locals.get_mut(i as usize).ok_or(WasmError::Trap("local index"))? = v;
                    }
                    Op::LocalTee(i) => {
                        let v = *stack.last().ok_or(WasmError::Trap("stack"))?;
                        *f.locals.get_mut(i as usize).ok_or(WasmError::Trap("local index"))? = v;
                    }
                    Op::GlobalGet(i) => stack.push(*self.globals.get(i as usize).unwrap_or(&0)),
                    Op::GlobalSet(i) => {
                        let v = stack.pop().ok_or(WasmError::Trap("stack"))?;
                        if let Some(g) = self.globals.get_mut(i as usize) {
                            *g = v;
                        }
                    }
                    Op::I32Const(v) => stack.push(v as i64),
                    Op::I64Const(v) => stack.push(v),
                    Op::F32Const(bits) => stack.push(bits as i64),
                    Op::F64Const(bits) => stack.push(bits as i64),
                    Op::I32Load(off) => {
                        let addr = stack.pop().ok_or(WasmError::Trap("stack"))? as u32 as usize + off as usize;
                        if addr + 4 > self.mem.len() {
                            return Err(WasmError::Trap("load oob"));
                        }
                        let v = i32::from_le_bytes([
                            self.mem[addr],
                            self.mem[addr + 1],
                            self.mem[addr + 2],
                            self.mem[addr + 3],
                        ]);
                        stack.push(v as i64);
                    }
                    Op::I32Store(off) => {
                        let v = stack.pop().ok_or(WasmError::Trap("stack"))? as i32;
                        let addr = stack.pop().ok_or(WasmError::Trap("stack"))? as u32 as usize + off as usize;
                        if addr + 4 > self.mem.len() {
                            return Err(WasmError::Trap("store oob"));
                        }
                        self.mem[addr..addr + 4].copy_from_slice(&v.to_le_bytes());
                    }
                    Op::MemorySize => stack.push((self.mem.len() / PAGE) as i64),
                    Op::MemoryGrow => {
                        let delta = stack.pop().ok_or(WasmError::Trap("stack"))? as u32 as usize;
                        let old = self.mem.len() / PAGE;
                        // Begrens de groei (audit H6): respecteer een harde pagina-plafond
                        // zodat een gemaakte module de kernel-allocator niet kan uitputten,
                        // en vermijd overloop in de groottenberekening. -1 = mislukt.
                        if delta > MAX_MEM_PAGES || old + delta > MAX_MEM_PAGES {
                            stack.push(-1);
                        } else {
                            self.mem.resize((old + delta) * PAGE, 0);
                            stack.push(old as i64);
                        }
                    }
                    Op::Num(opc) => exec_num(opc, &mut stack)?,
                }
            }

            // Frame klaar: resultaten naar de oproeper.
            let done = frames.pop().unwrap();
            let func = &self.m.funcs[done.code_idx];
            let nr = func.n_results as usize;
            // Houd de bovenste `nr` waarden (de resultaten), gooi de rest van dit
            // frame's stack weg tot sp_base.
            let keep_from = stack.len().saturating_sub(nr);
            let results: Vec<i64> = stack.split_off(keep_from);
            stack.truncate(done.sp_base);
            stack.extend(results.iter().copied());
            if frames.is_empty() {
                return Ok(results);
            }
        }
    }

    fn make_frame(&self, code_idx: usize, args: &[Val], sp_base: usize) -> Result<Frame, WasmError> {
        // Begrens de functie-index (audit H5): een `call N` met out-of-range N mag
        // de interpreter niet laten panieken — geef een nette trap.
        let func = self.m.funcs.get(code_idx).ok_or(WasmError::Trap("func index"))?;
        let mut locals = vec![0i64; (func.n_params + func.n_locals) as usize];
        for (i, a) in args.iter().enumerate().take(func.n_params as usize) {
            locals[i] = *a;
        }
        Ok(Frame {
            code_idx,
            ip: 0,
            locals,
            ctrl: Vec::new(),
            sp_base,
        })
    }

    fn fn_arity(&self, gidx: u32) -> (usize, usize) {
        let n_imp = self.m.n_imports();
        if gidx < n_imp {
            match self.m.imports.get(gidx as usize) {
                Some(imp) => (imp.n_params as usize, imp.n_results as usize),
                None => (0, 0),
            }
        } else {
            // Out-of-range func-index → (0,0); de echte trap volgt in make_frame (audit H5).
            match self.m.funcs.get((gidx - n_imp) as usize) {
                Some(f) => (f.n_params as usize, f.n_results as usize),
                None => (0, 0),
            }
        }
    }
}

fn do_branch(f: &mut Frame, stack: &mut Vec<i64>, depth: u32) -> Result<(), WasmError> {
    if depth as usize >= f.ctrl.len() {
        // Branch voorbij de buitenste control = functie-return.
        f.ip = u32::MAX as usize;
        return Ok(());
    }
    let idx = f.ctrl.len() - 1 - depth as usize;
    let e = f.ctrl[idx];
    // Behoud de bovenste `arity` waarden als resultaat van het blok.
    let keep: Vec<i64> = if e.arity > 0 {
        let from = stack.len().saturating_sub(e.arity as usize);
        stack.split_off(from)
    } else {
        Vec::new()
    };
    stack.truncate(e.height);
    stack.extend(keep);
    if e.is_loop {
        f.ctrl.truncate(idx + 1); // loop-entry blijft (her-iteratie)
    } else {
        f.ctrl.truncate(idx);
    }
    f.ip = e.target as usize;
    Ok(())
}

fn i32v(v: i64) -> i32 {
    v as i32
}

fn exec_num(op: u8, st: &mut Vec<i64>) -> Result<(), WasmError> {
    macro_rules! pop {
        () => {
            st.pop().ok_or(WasmError::Trap("stack"))?
        };
    }
    macro_rules! bin_i32 {
        ($f:expr) => {{
            let b = i32v(pop!());
            let a = i32v(pop!());
            let r: i32 = $f(a, b);
            st.push(r as i64);
        }};
    }
    macro_rules! cmp_i32 {
        ($f:expr) => {{
            let b = i32v(pop!());
            let a = i32v(pop!());
            st.push(if $f(a, b) { 1 } else { 0 });
        }};
    }
    macro_rules! bin_i64 {
        ($f:expr) => {{
            let b = pop!();
            let a = pop!();
            let r: i64 = $f(a, b);
            st.push(r);
        }};
    }
    macro_rules! cmp_i64 {
        ($f:expr) => {{
            let b = pop!();
            let a = pop!();
            st.push(if $f(a, b) { 1 } else { 0 });
        }};
    }
    match op {
        0x45 => {
            let a = i32v(pop!());
            st.push(if a == 0 { 1 } else { 0 });
        }
        0x46 => cmp_i32!(|a, b| a == b),
        0x47 => cmp_i32!(|a: i32, b: i32| a != b),
        0x48 => cmp_i32!(|a: i32, b: i32| a < b),
        0x49 => cmp_i32!(|a: i32, b: i32| (a as u32) < (b as u32)),
        0x4a => cmp_i32!(|a: i32, b: i32| a > b),
        0x4b => cmp_i32!(|a: i32, b: i32| (a as u32) > (b as u32)),
        0x4c => cmp_i32!(|a: i32, b: i32| a <= b),
        0x4d => cmp_i32!(|a: i32, b: i32| (a as u32) <= (b as u32)),
        0x4e => cmp_i32!(|a: i32, b: i32| a >= b),
        0x4f => cmp_i32!(|a: i32, b: i32| (a as u32) >= (b as u32)),
        0x6a => bin_i32!(|a: i32, b: i32| a.wrapping_add(b)),
        0x6b => bin_i32!(|a: i32, b: i32| a.wrapping_sub(b)),
        0x6c => bin_i32!(|a: i32, b: i32| a.wrapping_mul(b)),
        0x6d => {
            let b = i32v(pop!());
            let a = i32v(pop!());
            if b == 0 {
                return Err(WasmError::Trap("div0"));
            }
            st.push(a.wrapping_div(b) as i64);
        }
        0x6e => {
            let b = i32v(pop!()) as u32;
            let a = i32v(pop!()) as u32;
            if b == 0 {
                return Err(WasmError::Trap("div0"));
            }
            st.push((a / b) as i32 as i64);
        }
        0x6f => {
            let b = i32v(pop!());
            let a = i32v(pop!());
            if b == 0 {
                return Err(WasmError::Trap("rem0"));
            }
            st.push(a.wrapping_rem(b) as i64);
        }
        0x70 => {
            let b = i32v(pop!()) as u32;
            let a = i32v(pop!()) as u32;
            if b == 0 {
                return Err(WasmError::Trap("rem0"));
            }
            st.push((a % b) as i32 as i64);
        }
        0x71 => bin_i32!(|a: i32, b: i32| a & b),
        0x72 => bin_i32!(|a: i32, b: i32| a | b),
        0x73 => bin_i32!(|a: i32, b: i32| a ^ b),
        0x74 => bin_i32!(|a: i32, b: i32| a.wrapping_shl(b as u32)),
        0x75 => bin_i32!(|a: i32, b: i32| a.wrapping_shr(b as u32)),
        0x76 => bin_i32!(|a: i32, b: i32| ((a as u32).wrapping_shr(b as u32)) as i32),
        0x77 => bin_i32!(|a: i32, b: i32| a.rotate_left(b as u32)),
        0x78 => bin_i32!(|a: i32, b: i32| a.rotate_right(b as u32)),
        // i64
        0x50 => {
            let a = pop!();
            st.push(if a == 0 { 1 } else { 0 });
        }
        0x51 => cmp_i64!(|a, b| a == b),
        0x52 => cmp_i64!(|a: i64, b: i64| a != b),
        0x53 => cmp_i64!(|a: i64, b: i64| a < b),
        0x54 => cmp_i64!(|a: i64, b: i64| (a as u64) < (b as u64)),
        0x55 => cmp_i64!(|a: i64, b: i64| a > b),
        0x56 => cmp_i64!(|a: i64, b: i64| (a as u64) > (b as u64)),
        0x57 => cmp_i64!(|a: i64, b: i64| a <= b),
        0x58 => cmp_i64!(|a: i64, b: i64| (a as u64) <= (b as u64)),
        0x59 => cmp_i64!(|a: i64, b: i64| a >= b),
        0x5a => cmp_i64!(|a: i64, b: i64| (a as u64) >= (b as u64)),
        0x7c => bin_i64!(|a: i64, b: i64| a.wrapping_add(b)),
        0x7d => bin_i64!(|a: i64, b: i64| a.wrapping_sub(b)),
        0x7e => bin_i64!(|a: i64, b: i64| a.wrapping_mul(b)),
        0x83 => bin_i64!(|a: i64, b: i64| a & b),
        0x84 => bin_i64!(|a: i64, b: i64| a | b),
        0x85 => bin_i64!(|a: i64, b: i64| a ^ b),
        0x86 => bin_i64!(|a: i64, b: i64| a.wrapping_shl(b as u32)),
        0x87 => bin_i64!(|a: i64, b: i64| a.wrapping_shr(b as u32)),
        // conversies
        0xa7 => {
            let a = pop!();
            st.push(a as i32 as i64); // i32.wrap_i64
        }
        0xac => {
            let a = i32v(pop!());
            st.push(a as i64); // i64.extend_i32_s
        }
        0xad => {
            let a = i32v(pop!()) as u32;
            st.push(a as i64); // i64.extend_i32_u
        }
        // ── f32/f64 ── (no_std: geen libm → sqrt/ceil/floor/nearest niet ondersteund;
        // abs/neg via bit-manipulatie, rekenkunde via core-operatoren).
        0x5b => fcmp_f32(st, |a, b| a == b)?,
        0x5c => fcmp_f32(st, |a, b| a != b)?,
        0x5d => fcmp_f32(st, |a, b| a < b)?,
        0x5e => fcmp_f32(st, |a, b| a > b)?,
        0x5f => fcmp_f32(st, |a, b| a <= b)?,
        0x60 => fcmp_f32(st, |a, b| a >= b)?,
        0x61 => fcmp_f64(st, |a, b| a == b)?,
        0x62 => fcmp_f64(st, |a, b| a != b)?,
        0x63 => fcmp_f64(st, |a, b| a < b)?,
        0x64 => fcmp_f64(st, |a, b| a > b)?,
        0x65 => fcmp_f64(st, |a, b| a <= b)?,
        0x66 => fcmp_f64(st, |a, b| a >= b)?,
        0x8b => {
            let v = pop!() as u32 & 0x7fff_ffff; // f32.abs
            st.push(v as i64);
        }
        0x8c => {
            let v = pop!() as u32 ^ 0x8000_0000; // f32.neg
            st.push(v as i64);
        }
        0x92 => fbin_f32(st, |a, b| a + b)?,
        0x93 => fbin_f32(st, |a, b| a - b)?,
        0x94 => fbin_f32(st, |a, b| a * b)?,
        0x95 => fbin_f32(st, |a, b| a / b)?,
        0x96 => fbin_f32(st, |a, b| if a < b { a } else { b })?,
        0x97 => fbin_f32(st, |a, b| if a > b { a } else { b })?,
        0x99 => {
            let v = (pop!() as u64) & 0x7fff_ffff_ffff_ffff; // f64.abs
            st.push(v as i64);
        }
        0x9a => {
            let v = (pop!() as u64) ^ 0x8000_0000_0000_0000; // f64.neg
            st.push(v as i64);
        }
        0xa0 => fbin_f64(st, |a, b| a + b)?,
        0xa1 => fbin_f64(st, |a, b| a - b)?,
        0xa2 => fbin_f64(st, |a, b| a * b)?,
        0xa3 => fbin_f64(st, |a, b| a / b)?,
        0xa4 => fbin_f64(st, |a, b| if a < b { a } else { b })?,
        0xa5 => fbin_f64(st, |a, b| if a > b { a } else { b })?,
        // conversies float↔int
        0xa8 => {
            let a = f32::from_bits(pop!() as u32);
            st.push(a as i32 as i64); // i32.trunc_f32_s
        }
        0xaa => {
            let a = f64::from_bits(pop!() as u64);
            st.push(a as i32 as i64); // i32.trunc_f64_s
        }
        0xb2 => {
            let a = i32v(pop!());
            st.push((a as f32).to_bits() as u32 as i64); // f32.convert_i32_s
        }
        0xb6 => {
            let a = f64::from_bits(pop!() as u64);
            st.push((a as f32).to_bits() as u32 as i64); // f32.demote_f64
        }
        0xb7 => {
            let a = i32v(pop!());
            st.push((a as f64).to_bits() as i64); // f64.convert_i32_s
        }
        0xb8 => {
            let a = i32v(pop!()) as u32;
            st.push((a as f64).to_bits() as i64); // f64.convert_i32_u
        }
        0xbb => {
            let a = f32::from_bits(pop!() as u32);
            st.push((a as f64).to_bits() as i64); // f64.promote_f32
        }
        0xbc => {
            let v = pop!() as u32;
            st.push(v as i32 as i64); // i32.reinterpret_f32
        }
        0xbe => {
            let v = pop!() as u32;
            st.push(v as i64); // f32.reinterpret_i32
        }
        0xbd | 0xbf => { /* i64/f64 reinterpret: bits blijven gelijk → no-op */ }
        _ => return Err(WasmError::Unsupported("num-opcode")),
    }
    Ok(())
}

fn fbin_f64(st: &mut Vec<i64>, f: impl Fn(f64, f64) -> f64) -> Result<(), WasmError> {
    let b = f64::from_bits(st.pop().ok_or(WasmError::Trap("stack"))? as u64);
    let a = f64::from_bits(st.pop().ok_or(WasmError::Trap("stack"))? as u64);
    st.push(f(a, b).to_bits() as i64);
    Ok(())
}
fn fcmp_f64(st: &mut Vec<i64>, f: impl Fn(f64, f64) -> bool) -> Result<(), WasmError> {
    let b = f64::from_bits(st.pop().ok_or(WasmError::Trap("stack"))? as u64);
    let a = f64::from_bits(st.pop().ok_or(WasmError::Trap("stack"))? as u64);
    st.push(if f(a, b) { 1 } else { 0 });
    Ok(())
}
fn fbin_f32(st: &mut Vec<i64>, f: impl Fn(f32, f32) -> f32) -> Result<(), WasmError> {
    let b = f32::from_bits(st.pop().ok_or(WasmError::Trap("stack"))? as u32);
    let a = f32::from_bits(st.pop().ok_or(WasmError::Trap("stack"))? as u32);
    st.push(f(a, b).to_bits() as u32 as i64);
    Ok(())
}
fn fcmp_f32(st: &mut Vec<i64>, f: impl Fn(f32, f32) -> bool) -> Result<(), WasmError> {
    let b = f32::from_bits(st.pop().ok_or(WasmError::Trap("stack"))? as u32);
    let a = f32::from_bits(st.pop().ok_or(WasmError::Trap("stack"))? as u32);
    st.push(if f(a, b) { 1 } else { 0 });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Hand-geassembleerde WASM-modules ──
    fn u(mut n: u32) -> Vec<u8> {
        // uleb128
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
        s.extend(u(content.len() as u32));
        s.extend(content);
        s
    }
    fn header() -> Vec<u8> {
        vec![0, 0x61, 0x73, 0x6d, 1, 0, 0, 0]
    }

    /// () -> i32 { 42 }
    fn mod_answer() -> Vec<u8> {
        let mut w = header();
        // type: () -> (i32)
        w.extend(section(1, vec![1, 0x60, 0, 1, 0x7f]));
        w.extend(section(3, vec![1, 0])); // func 0 : type 0
        // export "answer" func 0
        let mut ex = vec![1, 6];
        ex.extend_from_slice(b"answer");
        ex.extend_from_slice(&[0, 0]);
        w.extend(section(7, ex));
        // code: i32.const 42; end
        w.extend(section(10, vec![1, 4, 0, 0x41, 42, 0x0b]));
        w
    }

    /// (i32,i32)->i32 { a+b }
    fn mod_add() -> Vec<u8> {
        let mut w = header();
        w.extend(section(1, vec![1, 0x60, 2, 0x7f, 0x7f, 1, 0x7f]));
        w.extend(section(3, vec![1, 0]));
        let mut ex = vec![1, 3];
        ex.extend_from_slice(b"add");
        ex.extend_from_slice(&[0, 0]);
        w.extend(section(7, ex));
        // local.get 0; local.get 1; i32.add; end
        w.extend(section(10, vec![1, 7, 0, 0x20, 0, 0x20, 1, 0x6a, 0x0b]));
        w
    }

    /// (i32 n)->i32: som 1..=n via een loop. Lokaal 1 = accumulator, lokaal 2 = i.
    fn mod_sum() -> Vec<u8> {
        let mut w = header();
        w.extend(section(1, vec![1, 0x60, 1, 0x7f, 1, 0x7f])); // (i32)->i32
        w.extend(section(3, vec![1, 0]));
        let mut ex = vec![1, 3];
        ex.extend_from_slice(b"sum");
        ex.extend_from_slice(&[0, 0]);
        w.extend(section(7, ex));
        // 2 extra locals (i32): acc(1), i(2)
        // body:
        //  block            ;; depth target = na block (exit)
        //   loop             ;; depth target = loop-start
        //    local.get 2; local.get 0; i32.gt_s; br_if 1   ;; if i>n exit block
        //    local.get 1; local.get 2; i32.add; local.set 1 ;; acc += i
        //    local.get 2; i32.const 1; i32.add; local.set 2 ;; i += 1
        //    br 0           ;; loop
        //   end
        //  end
        //  local.get 1; end
        let mut code = Vec::new();
        code.push(1); // 1 local-group
        code.extend_from_slice(&[2, 0x7f]); // 2 locals i32
        code.extend_from_slice(&[
            0x41, 1, 0x21, 2, // i = 1
            0x02, 0x40, // block void
            0x03, 0x40, // loop void
            0x20, 2, 0x20, 0, 0x4a, 0x0d, 1, // if i>n: br 1 (exit block)
            0x20, 1, 0x20, 2, 0x6a, 0x21, 1, // acc += i
            0x20, 2, 0x41, 1, 0x6a, 0x21, 2, // i += 1
            0x0c, 0, // br 0 (loop)
            0x0b, // end loop
            0x0b, // end block
            0x20, 1, // local.get acc
            0x0b, // end func
        ]);
        let mut full = vec![1u8];
        full.extend(u(code.len() as u32));
        full.extend(code);
        w.extend(section(10, full));
        w
    }

    /// factorial via recursie: fac(n) = n<2 ? 1 : n*fac(n-1)
    fn mod_fac() -> Vec<u8> {
        let mut w = header();
        w.extend(section(1, vec![1, 0x60, 1, 0x7f, 1, 0x7f]));
        w.extend(section(3, vec![1, 0]));
        let mut ex = vec![1, 3];
        ex.extend_from_slice(b"fac");
        ex.extend_from_slice(&[0, 0]);
        w.extend(section(7, ex));
        // local.get 0; i32.const 2; i32.lt_s; if (result i32) i32.const 1
        //   else local.get 0; local.get 0; i32.const 1; i32.sub; call 0; i32.mul end ; end
        let body = vec![
            0u8, // geen extra locals
            0x20, 0, 0x41, 2, 0x48, // n < 2
            0x04, 0x7f, // if (result i32)
            0x41, 1, // then 1
            0x05, // else
            0x20, 0, 0x20, 0, 0x41, 1, 0x6b, 0x10, 0, 0x6c, // n * fac(n-1)
            0x0b, // end if
            0x0b, // end func
        ];
        let mut full = vec![1u8];
        full.extend(u(body.len() as u32));
        full.extend(body);
        w.extend(section(10, full));
        w
    }

    #[test]
    fn parse_and_run_answer() {
        let m = Module::parse(&mod_answer()).unwrap();
        let mut inst = Instance::new(&m);
        let r = inst.invoke("answer", &[], &mut NoImports).unwrap();
        assert_eq!(r, vec![42]);
    }

    #[test]
    fn run_add() {
        let m = Module::parse(&mod_add()).unwrap();
        let mut inst = Instance::new(&m);
        assert_eq!(inst.invoke("add", &[20, 22], &mut NoImports).unwrap(), vec![42]);
        assert_eq!(inst.invoke("add", &[-5, 8], &mut NoImports).unwrap(), vec![3]);
    }

    #[test]
    fn run_sum_loop() {
        let m = Module::parse(&mod_sum()).unwrap();
        let mut inst = Instance::new(&m);
        // som 1..=100 = 5050
        assert_eq!(inst.invoke("sum", &[100], &mut NoImports).unwrap(), vec![5050]);
        assert_eq!(inst.invoke("sum", &[10], &mut NoImports).unwrap(), vec![55]);
        assert_eq!(inst.invoke("sum", &[0], &mut NoImports).unwrap(), vec![0]);
    }

    #[test]
    fn run_factorial_recursive() {
        let m = Module::parse(&mod_fac()).unwrap();
        let mut inst = Instance::new(&m);
        assert_eq!(inst.invoke("fac", &[5], &mut NoImports).unwrap(), vec![120]);
        assert_eq!(inst.invoke("fac", &[1], &mut NoImports).unwrap(), vec![1]);
        assert_eq!(inst.invoke("fac", &[6], &mut NoImports).unwrap(), vec![720]);
    }

    /// (i32 n)->i32 : trunc(n_as_f64 * 2.5 + 0.5) — oefent f64.convert/mul/add/trunc.
    fn mod_float() -> Vec<u8> {
        let mut w = header();
        w.extend(section(1, vec![1, 0x60, 1, 0x7f, 1, 0x7f]));
        w.extend(section(3, vec![1, 0]));
        let mut ex = vec![1, 5];
        ex.extend_from_slice(b"fcomp");
        ex.extend_from_slice(&[0, 0]);
        w.extend(section(7, ex));
        let mut body = vec![0u8]; // geen locals
        body.extend_from_slice(&[0x20, 0, 0xb7]); // local.get 0; f64.convert_i32_s
        body.push(0x44); // f64.const 2.5
        body.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0x04, 0x40]);
        body.push(0xa2); // f64.mul
        body.push(0x44); // f64.const 0.5
        body.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0xe0, 0x3f]);
        body.push(0xa0); // f64.add
        body.extend_from_slice(&[0xaa, 0x0b]); // i32.trunc_f64_s; end
        let mut full = vec![1u8];
        full.extend(u(body.len() as u32));
        full.extend(body);
        w.extend(section(10, full));
        w
    }

    #[test]
    fn run_f64_arithmetic() {
        let m = Module::parse(&mod_float()).unwrap();
        let mut inst = Instance::new(&m);
        assert_eq!(inst.invoke("fcomp", &[10], &mut NoImports).unwrap(), vec![25]); // 25.5→25
        assert_eq!(inst.invoke("fcomp", &[4], &mut NoImports).unwrap(), vec![10]); // 10.5→10
        assert_eq!(inst.invoke("fcomp", &[0], &mut NoImports).unwrap(), vec![0]); // 0.5→0
        assert_eq!(inst.invoke("fcomp", &[-2], &mut NoImports).unwrap(), vec![-4]); // -4.5→-4
    }

    #[test]
    fn no_such_export() {
        let m = Module::parse(&mod_answer()).unwrap();
        let mut inst = Instance::new(&m);
        assert_eq!(inst.invoke("nope", &[], &mut NoImports), Err(WasmError::NoSuchExport));
    }

    #[test]
    fn bad_magic_rejected() {
        assert!(matches!(Module::parse(&[1, 2, 3, 4]), Err(WasmError::BadMagic)));
    }

    // ── WASI-op-capabilities: een module die "euro"/"fd_write" importeert ──
    /// import "euro"."fd_write" (i32 ptr, i32 len)->i32 ; export "run"()->i32 die
    /// "hi" in geheugen zet en fd_write(ptr,len) aanroept.
    fn mod_wasi_write() -> Vec<u8> {
        let mut w = header();
        // types: 0 = (i32,i32)->i32 (fd_write), 1 = ()->i32 (run)
        w.extend(section(
            1,
            vec![2, 0x60, 2, 0x7f, 0x7f, 1, 0x7f, 0x60, 0, 1, 0x7f],
        ));
        // import euro.fd_write : type 0
        let mut im = vec![1];
        im.extend_from_slice(&[4]);
        im.extend_from_slice(b"euro");
        im.extend_from_slice(&[8]);
        im.extend_from_slice(b"fd_write");
        im.extend_from_slice(&[0x00, 0]); // func, type 0
        w.extend(section(2, im));
        // function: 1 gedefinieerde functie, type 1
        w.extend(section(3, vec![1, 1]));
        // memory: 1 pagina
        w.extend(section(5, vec![1, 0x00, 1]));
        // export "run" = func index 1 (import 0 + def 0)
        let mut ex = vec![1, 3];
        ex.extend_from_slice(b"run");
        ex.extend_from_slice(&[0, 1]);
        w.extend(section(7, ex));
        // code: i32.const 0; i32.const 0x4948 ('HI' LE); i32.store ; i32.const 0;
        //       i32.const 2; call 0 ; end
        let body = vec![
            0u8,
            0x41, 0, // addr 0
            0x41, 0xc8, 0x92, 0x01, // i32.const 0x4948 ('H'=0x48,'I'=0x49 LE) = 18760
            0x36, 0x02, 0, // i32.store align=2 off=0
            0x41, 0, 0x41, 2, // ptr=0, len=2
            0x10, 0, // call fd_write (import 0)
            0x0b,
        ];
        let mut full = vec![1u8];
        full.extend(u(body.len() as u32));
        full.extend(body);
        w.extend(section(10, full));
        w
    }

    struct CapHost {
        cap_file: bool,
        out: Vec<u8>,
    }
    impl HostImports for CapHost {
        fn call(&mut self, m: &str, n: &str, args: &[Val], mem: &mut [u8]) -> Result<Vec<Val>, WasmError> {
            if m == "euro" && n == "fd_write" {
                if !self.cap_file {
                    return Err(WasmError::CapabilityDenied("CAP_FILE voor fd_write".into()));
                }
                let ptr = args[0] as usize;
                let len = args[1] as usize;
                self.out.extend_from_slice(&mem[ptr..ptr + len]);
                return Ok(vec![len as i64]);
            }
            Err(WasmError::HostError("onbekend".into()))
        }
    }

    #[test]
    fn wasi_import_gated_by_capability() {
        let m = Module::parse(&mod_wasi_write()).unwrap();
        // Met CAP_FILE: de host-call slaagt en schrijft "HI".
        let mut inst = Instance::new(&m);
        let mut host = CapHost { cap_file: true, out: Vec::new() };
        let r = inst.invoke("run", &[], &mut host).unwrap();
        assert_eq!(r, vec![2]);
        assert_eq!(&host.out, b"HI");
        // Zonder CAP_FILE: de host weigert → de WASM-trap propageert.
        let mut inst2 = Instance::new(&m);
        let mut deny = CapHost { cap_file: false, out: Vec::new() };
        assert!(matches!(
            inst2.invoke("run", &[], &mut deny),
            Err(WasmError::CapabilityDenied(_))
        ));
    }
}
