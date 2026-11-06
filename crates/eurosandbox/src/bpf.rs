//! Classic BPF as seccomp uses it (`SECCOMP_SET_MODE_FILTER`): a tiny, bounded
//! virtual machine that looks at `struct seccomp_data` and returns an action.
//! No loops (jumps only go forward), at most 4096 instructions, every load
//! inside the 64-byte data block; a program that breaks a rule is refused at
//! load time, exactly like Linux. Pure logic, host-tested.

use alloc::vec::Vec;

/// One `struct sock_filter`: code, jt, jf, k.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Insn {
    pub code: u16,
    pub jt: u8,
    pub jf: u8,
    pub k: u32,
}

impl Insn {
    /// Decode the 8-byte on-wire form (`u16 code, u8 jt, u8 jf, u32 k`).
    pub fn from_bytes(b: &[u8]) -> Option<Insn> {
        if b.len() < 8 {
            return None;
        }
        Some(Insn { code: u16::from_le_bytes([b[0], b[1]]), jt: b[2], jf: b[3], k: u32::from_le_bytes([b[4], b[5], b[6], b[7]]) })
    }
}

/// What the kernel passes to the filter: `struct seccomp_data`.
#[derive(Clone, Copy, Debug, Default)]
pub struct SeccompData {
    pub nr: i32,
    pub arch: u32,
    pub instruction_pointer: u64,
    pub args: [u64; 6],
}

impl SeccompData {
    /// The 32-bit word at byte offset `off` of the C layout (nr, arch, ip, args).
    fn word(&self, off: u32) -> Option<u32> {
        if off % 4 != 0 || off >= 64 {
            return None;
        }
        let w = |v: u64, high: bool| if high { (v >> 32) as u32 } else { v as u32 };
        Some(match off {
            0 => self.nr as u32,
            4 => self.arch,
            8 => w(self.instruction_pointer, false),
            12 => w(self.instruction_pointer, true),
            o => {
                let i = ((o - 16) / 8) as usize;
                w(self.args[i], (o - 16) % 8 == 4)
            }
        })
    }
}

/// The seccomp return actions (high 16 bits) and their precedence: a lower
/// value wins when several filters are installed.
pub const RET_KILL_PROCESS: u32 = 0x8000_0000;
pub const RET_KILL_THREAD: u32 = 0x0000_0000;
pub const RET_TRAP: u32 = 0x0003_0000;
pub const RET_ERRNO: u32 = 0x0005_0000;
pub const RET_USER_NOTIF: u32 = 0x7fc0_0000;
pub const RET_TRACE: u32 = 0x7ff0_0000;
pub const RET_LOG: u32 = 0x7ffc_0000;
pub const RET_ALLOW: u32 = 0x7fff_0000;
pub const RET_ACTION_FULL: u32 = 0xffff_0000;
pub const RET_DATA: u32 = 0x0000_ffff;

pub const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;
pub const MAX_INSNS: usize = 4096;

/// Why a program was refused at load time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LoadError {
    Empty,
    TooLong,
    BadInstruction(usize),
    JumpOutOfRange(usize),
    LoadOutOfRange(usize),
    NoReturnAtEnd,
}

/// A validated filter program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    insns: Vec<Insn>,
}

// Instruction classes and modes (linux/filter.h).
const BPF_LD: u16 = 0x00;
const BPF_LDX: u16 = 0x01;
const BPF_ST: u16 = 0x02;
const BPF_STX: u16 = 0x03;
const BPF_ALU: u16 = 0x04;
const BPF_JMP: u16 = 0x05;
const BPF_RET: u16 = 0x06;
const BPF_MISC: u16 = 0x07;

const BPF_W: u16 = 0x00;
const BPF_IMM: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_IND: u16 = 0x40;
const BPF_MEM: u16 = 0x60;
const BPF_LEN: u16 = 0x80;

const BPF_K: u16 = 0x00;
const BPF_X: u16 = 0x08;
const BPF_A: u16 = 0x10;

const BPF_ADD: u16 = 0x00;
const BPF_SUB: u16 = 0x10;
const BPF_MUL: u16 = 0x20;
const BPF_DIV: u16 = 0x30;
const BPF_OR: u16 = 0x40;
const BPF_AND: u16 = 0x50;
const BPF_LSH: u16 = 0x60;
const BPF_RSH: u16 = 0x70;
const BPF_NEG: u16 = 0x80;
const BPF_MOD: u16 = 0x90;
const BPF_XOR: u16 = 0xa0;

const BPF_JA: u16 = 0x00;
const BPF_JEQ: u16 = 0x10;
const BPF_JGT: u16 = 0x20;
const BPF_JGE: u16 = 0x30;
const BPF_JSET: u16 = 0x40;

const BPF_TAX: u16 = 0x00;
const BPF_TXA: u16 = 0x80;

impl Filter {
    /// Validate a program the way the kernel does before installing it.
    pub fn load(insns: Vec<Insn>) -> Result<Filter, LoadError> {
        let n = insns.len();
        if n == 0 {
            return Err(LoadError::Empty);
        }
        if n > MAX_INSNS {
            return Err(LoadError::TooLong);
        }
        for (i, ins) in insns.iter().enumerate() {
            let class = ins.code & 0x07;
            match class {
                BPF_LD | BPF_LDX => {
                    let mode = ins.code & 0xe0;
                    let size = ins.code & 0x18;
                    match mode {
                        BPF_IMM | BPF_LEN => {}
                        BPF_ABS => {
                            // seccomp: only 32-bit, aligned loads inside seccomp_data.
                            if size != BPF_W || ins.k % 4 != 0 || ins.k >= 64 {
                                return Err(LoadError::LoadOutOfRange(i));
                            }
                        }
                        BPF_MEM => {
                            if ins.k >= 16 {
                                return Err(LoadError::LoadOutOfRange(i));
                            }
                        }
                        BPF_IND => return Err(LoadError::BadInstruction(i)), // not allowed by seccomp
                        _ => return Err(LoadError::BadInstruction(i)),
                    }
                }
                BPF_ST | BPF_STX => {
                    if ins.k >= 16 {
                        return Err(LoadError::LoadOutOfRange(i));
                    }
                }
                BPF_ALU => {
                    let op = ins.code & 0xf0;
                    if !matches!(op, BPF_ADD | BPF_SUB | BPF_MUL | BPF_DIV | BPF_OR | BPF_AND | BPF_LSH | BPF_RSH | BPF_NEG | BPF_MOD | BPF_XOR) {
                        return Err(LoadError::BadInstruction(i));
                    }
                    if matches!(op, BPF_DIV | BPF_MOD) && ins.code & BPF_X == 0 && ins.k == 0 {
                        return Err(LoadError::BadInstruction(i)); // division by a constant zero
                    }
                }
                BPF_JMP => {
                    let op = ins.code & 0xf0;
                    match op {
                        BPF_JA => {
                            if (ins.k as usize) >= n - i - 1 + 1 && i + 1 + ins.k as usize >= n {
                                return Err(LoadError::JumpOutOfRange(i));
                            }
                        }
                        BPF_JEQ | BPF_JGT | BPF_JGE | BPF_JSET => {
                            if i + 1 + ins.jt as usize >= n || i + 1 + ins.jf as usize >= n {
                                return Err(LoadError::JumpOutOfRange(i));
                            }
                        }
                        _ => return Err(LoadError::BadInstruction(i)),
                    }
                }
                BPF_RET => {
                    if !matches!(ins.code & 0x18, BPF_K | BPF_A) {
                        return Err(LoadError::BadInstruction(i));
                    }
                }
                BPF_MISC => {
                    if !matches!(ins.code & 0xf8, BPF_TAX | BPF_TXA) {
                        return Err(LoadError::BadInstruction(i));
                    }
                }
                _ => return Err(LoadError::BadInstruction(i)),
            }
        }
        let last = insns[n - 1];
        if last.code & 0x07 != BPF_RET && last.code & 0x07 != BPF_JMP {
            return Err(LoadError::NoReturnAtEnd);
        }
        if last.code & 0x07 == BPF_JMP {
            return Err(LoadError::NoReturnAtEnd); // a jump as the last instruction falls off the end
        }
        Ok(Filter { insns })
    }

    /// Decode and validate the on-wire `sock_filter[]` bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Filter, LoadError> {
        let mut v = Vec::with_capacity(bytes.len() / 8);
        for chunk in bytes.chunks_exact(8) {
            if let Some(i) = Insn::from_bytes(chunk) {
                v.push(i);
            }
        }
        Filter::load(v)
    }

    pub fn len(&self) -> usize {
        self.insns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.insns.is_empty()
    }

    /// Run the program over `data`; returns the raw seccomp return value.
    /// A validated program always terminates (forward jumps only).
    pub fn run(&self, data: &SeccompData) -> u32 {
        let mut a: u32 = 0;
        let mut x: u32 = 0;
        let mut mem = [0u32; 16];
        let mut pc = 0usize;
        while pc < self.insns.len() {
            let ins = self.insns[pc];
            pc += 1;
            let class = ins.code & 0x07;
            match class {
                BPF_LD => {
                    a = match ins.code & 0xe0 {
                        BPF_IMM => ins.k,
                        BPF_ABS => match data.word(ins.k) {
                            Some(w) => w,
                            None => return RET_KILL_PROCESS,
                        },
                        BPF_MEM => mem[(ins.k as usize) & 15],
                        BPF_LEN => 64,
                        _ => return RET_KILL_PROCESS,
                    };
                }
                BPF_LDX => {
                    x = match ins.code & 0xe0 {
                        BPF_IMM => ins.k,
                        BPF_MEM => mem[(ins.k as usize) & 15],
                        BPF_LEN => 64,
                        _ => return RET_KILL_PROCESS,
                    };
                }
                BPF_ST => mem[(ins.k as usize) & 15] = a,
                BPF_STX => mem[(ins.k as usize) & 15] = x,
                BPF_ALU => {
                    let src = if ins.code & BPF_X != 0 { x } else { ins.k };
                    a = match ins.code & 0xf0 {
                        BPF_ADD => a.wrapping_add(src),
                        BPF_SUB => a.wrapping_sub(src),
                        BPF_MUL => a.wrapping_mul(src),
                        BPF_DIV => {
                            if src == 0 {
                                return RET_KILL_PROCESS;
                            }
                            a / src
                        }
                        BPF_MOD => {
                            if src == 0 {
                                return RET_KILL_PROCESS;
                            }
                            a % src
                        }
                        BPF_OR => a | src,
                        BPF_AND => a & src,
                        BPF_XOR => a ^ src,
                        BPF_LSH => a.checked_shl(src).unwrap_or(0),
                        BPF_RSH => a.checked_shr(src).unwrap_or(0),
                        BPF_NEG => a.wrapping_neg(),
                        _ => return RET_KILL_PROCESS,
                    };
                }
                BPF_JMP => {
                    let src = if ins.code & BPF_X != 0 { x } else { ins.k };
                    let taken = match ins.code & 0xf0 {
                        BPF_JA => {
                            pc += ins.k as usize;
                            continue;
                        }
                        BPF_JEQ => a == src,
                        BPF_JGT => a > src,
                        BPF_JGE => a >= src,
                        BPF_JSET => a & src != 0,
                        _ => return RET_KILL_PROCESS,
                    };
                    pc += if taken { ins.jt as usize } else { ins.jf as usize };
                }
                BPF_RET => {
                    return if ins.code & 0x18 == BPF_A { a } else { ins.k };
                }
                BPF_MISC => match ins.code & 0xf8 {
                    BPF_TAX => x = a,
                    BPF_TXA => a = x,
                    _ => return RET_KILL_PROCESS,
                },
                _ => return RET_KILL_PROCESS,
            }
        }
        RET_KILL_PROCESS // fell off the end: cannot happen after load()
    }
}

/// Evaluate every installed filter (newest first, like Linux) and return the
/// most restrictive action: the lowest `action` value wins, data from that filter.
pub fn evaluate(filters: &[Filter], data: &SeccompData) -> u32 {
    let mut best = RET_ALLOW;
    for f in filters.iter().rev() {
        let r = f.run(data);
        if precedence(r) < precedence(best) {
            best = r;
        }
    }
    best
}

/// Linux's action precedence (lower wins). KILL_PROCESS is numerically the
/// largest value but the strongest action, so this is a table, not a compare.
fn precedence(ret: u32) -> u8 {
    match ret & RET_ACTION_FULL {
        RET_KILL_PROCESS => 0,
        RET_KILL_THREAD => 1,
        RET_TRAP => 2,
        RET_ERRNO => 3,
        RET_USER_NOTIF => 4,
        RET_TRACE => 5,
        RET_LOG => 6,
        RET_ALLOW => 7,
        _ => 0, // an unknown action is treated as a kill, never as an allow
    }
}

/// What the kernel should do with a raw return value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Allow,
    Log,
    /// Fail the syscall with this errno (positive).
    Errno(u16),
    /// SIGSYS to the thread (we end the process; data carries the signal info).
    Trap,
    KillThread,
    KillProcess,
    /// TRACE / USER_NOTIF: no tracer or supervisor here; Linux without a tracer
    /// treats TRACE as ENOSYS.
    Enosys,
}

pub fn action_of(ret: u32) -> Action {
    match ret & RET_ACTION_FULL {
        RET_ALLOW => Action::Allow,
        RET_LOG => Action::Log,
        RET_ERRNO => Action::Errno((ret & RET_DATA) as u16),
        RET_TRAP => Action::Trap,
        RET_KILL_THREAD => Action::KillThread,
        RET_KILL_PROCESS => Action::KillProcess,
        _ => Action::Enosys,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stmt(code: u16, k: u32) -> Insn {
        Insn { code, jt: 0, jf: 0, k }
    }
    fn jump(code: u16, k: u32, jt: u8, jf: u8) -> Insn {
        Insn { code, jt, jf, k }
    }
    const LD_W_ABS: u16 = BPF_LD | BPF_W | BPF_ABS;
    const JEQ_K: u16 = BPF_JMP | BPF_JEQ | BPF_K;
    const RET_K: u16 = BPF_RET | BPF_K;

    /// The textbook seccomp program: check arch, then allow or deny by nr.
    fn deny_getpid_kill_socket() -> Filter {
        Filter::load(alloc::vec![
            stmt(LD_W_ABS, 4),                                   // A = arch
            jump(JEQ_K, AUDIT_ARCH_X86_64, 1, 0),                // if arch == x86_64 skip 1
            stmt(RET_K, RET_KILL_PROCESS),                       // else kill
            stmt(LD_W_ABS, 0),                                   // A = nr
            jump(JEQ_K, 39, 0, 1),                               // getpid?
            stmt(RET_K, RET_ERRNO | 1),                          //   -> EPERM
            jump(JEQ_K, 41, 0, 1),                               // socket?
            stmt(RET_K, RET_KILL_PROCESS),                       //   -> kill
            stmt(RET_K, RET_ALLOW),
        ])
        .unwrap()
    }

    fn data(nr: i32) -> SeccompData {
        SeccompData { nr, arch: AUDIT_ARCH_X86_64, instruction_pointer: 0x1000, args: [0; 6] }
    }

    #[test]
    fn allows_denies_and_kills_by_syscall_number() {
        let f = deny_getpid_kill_socket();
        assert_eq!(action_of(f.run(&data(0))), Action::Allow);
        assert_eq!(action_of(f.run(&data(39))), Action::Errno(1));
        assert_eq!(action_of(f.run(&data(41))), Action::KillProcess);
        // wrong architecture is killed before any nr check
        let mut d = data(0);
        d.arch = 0x4000_003e;
        assert_eq!(action_of(f.run(&d)), Action::KillProcess);
    }

    #[test]
    fn argument_loads_see_both_halves() {
        // deny socket(AF_INET=2) but allow socket(AF_UNIX=1): args[0] low word at offset 16
        let f = Filter::load(alloc::vec![
            stmt(LD_W_ABS, 0),
            jump(JEQ_K, 41, 0, 3),
            stmt(LD_W_ABS, 16),
            jump(JEQ_K, 2, 0, 1),
            stmt(RET_K, RET_ERRNO | 13),
            stmt(RET_K, RET_ALLOW),
        ])
        .unwrap();
        let mut d = data(41);
        d.args[0] = 2;
        assert_eq!(action_of(f.run(&d)), Action::Errno(13));
        d.args[0] = 1;
        assert_eq!(action_of(f.run(&d)), Action::Allow);
        d.args[0] = 2 | (7 << 32); // high half must not leak into the low word
        assert_eq!(action_of(f.run(&d)), Action::Errno(13));
        let f2 = Filter::load(alloc::vec![stmt(LD_W_ABS, 20), stmt(BPF_RET | BPF_A, 0)]).unwrap();
        assert_eq!(f2.run(&d), 7);
    }

    #[test]
    fn alu_and_scratch_memory_work() {
        // A = nr; A = A * 2 + 1; M[3] = A; A = 0; A = M[3]; X = A; return X via TXA
        let f = Filter::load(alloc::vec![
            stmt(LD_W_ABS, 0),
            stmt(BPF_ALU | BPF_MUL | BPF_K, 2),
            stmt(BPF_ALU | BPF_ADD | BPF_K, 1),
            stmt(BPF_ST, 3),
            stmt(BPF_LD | BPF_IMM, 0),
            stmt(BPF_LD | BPF_MEM, 3),
            stmt(BPF_MISC | BPF_TAX, 0),
            stmt(BPF_LD | BPF_IMM, 0),
            stmt(BPF_MISC | BPF_TXA, 0),
            stmt(BPF_RET | BPF_A, 0),
        ])
        .unwrap();
        assert_eq!(f.run(&data(20)), 41);
    }

    #[test]
    fn refuses_bad_programs_at_load() {
        assert_eq!(Filter::load(alloc::vec![]), Err(LoadError::Empty));
        assert_eq!(Filter::load(alloc::vec![stmt(LD_W_ABS, 0)]), Err(LoadError::NoReturnAtEnd));
        assert_eq!(Filter::load(alloc::vec![jump(JEQ_K, 1, 5, 0), stmt(RET_K, RET_ALLOW)]), Err(LoadError::JumpOutOfRange(0)));
        assert_eq!(Filter::load(alloc::vec![stmt(LD_W_ABS, 66), stmt(RET_K, RET_ALLOW)]), Err(LoadError::LoadOutOfRange(0)));
        assert_eq!(Filter::load(alloc::vec![stmt(LD_W_ABS, 2), stmt(RET_K, RET_ALLOW)]), Err(LoadError::LoadOutOfRange(0)));
        assert_eq!(Filter::load(alloc::vec![stmt(BPF_LD | BPF_W | BPF_IND, 0), stmt(RET_K, RET_ALLOW)]), Err(LoadError::BadInstruction(0)));
        assert_eq!(Filter::load(alloc::vec![stmt(BPF_ALU | BPF_DIV | BPF_K, 0), stmt(RET_K, RET_ALLOW)]), Err(LoadError::BadInstruction(0)));
        let too_long: Vec<Insn> = (0..MAX_INSNS + 1).map(|_| stmt(RET_K, RET_ALLOW)).collect();
        assert_eq!(Filter::load(too_long), Err(LoadError::TooLong));
    }

    #[test]
    fn wire_format_round_trips() {
        let f = deny_getpid_kill_socket();
        let mut bytes = Vec::new();
        for i in &f.insns {
            bytes.extend_from_slice(&i.code.to_le_bytes());
            bytes.push(i.jt);
            bytes.push(i.jf);
            bytes.extend_from_slice(&i.k.to_le_bytes());
        }
        let g = Filter::from_bytes(&bytes).unwrap();
        assert_eq!(f, g);
    }

    #[test]
    fn most_restrictive_filter_wins() {
        let allow_all = Filter::load(alloc::vec![stmt(RET_K, RET_ALLOW)]).unwrap();
        let strict = deny_getpid_kill_socket();
        assert_eq!(action_of(evaluate(&[allow_all.clone(), strict.clone()], &data(39))), Action::Errno(1));
        assert_eq!(action_of(evaluate(&[strict, allow_all], &data(41))), Action::KillProcess);
        assert_eq!(action_of(evaluate(&[], &data(41))), Action::Allow);
    }
}
