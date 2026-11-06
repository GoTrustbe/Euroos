//! efuzz: the Euro API under fire. 20000 calls to random Euro numbers
//! (0x6100..=0x610C, plus a few just outside the range) with arguments drawn
//! from a pool of hostile values (null, huge, unaligned, kernel addresses,
//! negative) and from this app's own memory. Deterministic (xorshift). The
//! kernel must answer every call with a value or an error and never fault;
//! the `[eurofuzz]` self-test checks that this program reaches exit 171.
//! It runs with `console,window,file`, so every arm is exercised, including
//! surface_present and text_draw with absurd sizes, and the file calls with
//! paths outside the scope. The pool also holds the app's own code page: a
//! write there through the kernel must be refused (-EFAULT), never faulted.
#![no_std]
#![no_main]

static mut BUF: [u8; 65536] = [0; 65536];

fn main() -> i32 {
    let _ = eurosdk::log("efuzz: 20000 random Euro calls starting");
    let buf = unsafe { core::ptr::addr_of_mut!(BUF) as u64 };
    // This app's own CODE page (mapped R-X) and its stack: a kernel that wrote
    // through such a pointer would fault in ring 0; the answer must be -EFAULT.
    let code = main as *const () as u64;
    let stack = &buf as *const u64 as u64;
    let pool: [u64; 24] = [
        0, 1, 2, 7, 0xff, 0x1000, 0xffff, u64::MAX, u64::MAX - 1, 0x7fff_ffff, 0xffff_ffff, 0x1_0000_0000,
        0x8000_0000_0000_0000, 0xffff_8000_0000_0000, buf, buf + 100, buf + 65535, buf + 65536, 0x2f00_0000, 1 << 40,
        code, code & !0xfff, stack, stack - 4096,
    ];
    let nrs: [u64; 17] = [
        0x6100, 0x6101, 0x6102, 0x6103, 0x6104, 0x6105, 0x6106, 0x6107, 0x6108, 0x6109, 0x610A, 0x610B,
        0x6100, 0x6102, 0x6105, 0x6103, 0x6106, // weighted: the pointer-heavy ones
    ];
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut rnd = || { x ^= x << 13; x ^= x >> 7; x ^= x << 17; x };
    for i in 0..20000u32 {
        let r = rnd();
        let nr = nrs[(r % nrs.len() as u64) as usize];
        let mut a = [0u64; 5];
        for (k, slot) in a.iter_mut().enumerate() {
            let q = rnd() >> (k * 3);
            *slot = if q & 3 == 0 { q } else { pool[((q >> 8) % pool.len() as u64) as usize] };
        }
        // event_wait/sleep with a huge timeout would be a long nap: keep them short.
        if nr == 0x6104 || nr == 0x6109 { a[0] %= 3; }
        unsafe {
            let _r: u64;
            core::arch::asm!("syscall", inlateout("rax") nr => _r, in("rdi") a[0], in("rsi") a[1], in("rdx") a[2],
                in("r10") a[3], in("r8") a[4], lateout("rcx") _, lateout("r11") _, options(nostack));
        }
        if i % 5000 == 4999 {
            let _ = eurosdk::log("efuzz: 5000 more calls survived");
        }
    }
    let _ = eurosdk::log("efuzz: done");
    171
}

eurosdk::entry!(main);
