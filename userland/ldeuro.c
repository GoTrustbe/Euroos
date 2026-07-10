/* EuroOS 3C-3 — ld-euro.so: a from-scratch USERSPACE dynamic linker.
 *
 * The kernel loads a PT_INTERP executable + this interpreter + libc-euro.so into
 * the address space, sets the auxiliary vector (AT_ENTRY/AT_PHDR/AT_PHNUM/AT_BASE
 * plus two EuroOS entries carrying the exe and libc load bases), and jumps HERE
 * instead of the exe. This code then does the actual dynamic linking IN USERSPACE
 * — exactly what glibc/musl ld.so does: it walks the exe's PT_DYNAMIC, applies
 * R_X86_64_RELATIVE, and resolves R_X86_64_JUMP_SLOT / GLOB_DAT symbols against
 * libc-euro.so — then jumps to the program's real entry point.
 *
 * Freestanding, no libc. The kernel bootstraps this module's own RELATIVE relocs
 * before entry (as a real ld.so self-relocates first). */

typedef unsigned long u64;
typedef unsigned int u32;
typedef unsigned short u16;

/* EuroOS auxv extensions carrying the load bases the interpreter needs. */
#define AT_NULL 0
#define AT_PHDR 3
#define AT_PHNUM 5
#define AT_ENTRY 9
#define AT_EURO_EXE_BASE 0x6E01
#define AT_EURO_LIBC_BASE 0x6E02

/* Dynamic tags / relocation types. */
#define DT_HASH 4
#define DT_STRTAB 5
#define DT_SYMTAB 6
#define DT_RELA 7
#define DT_RELASZ 8
#define DT_PLTRELSZ 2
#define DT_JMPREL 23
#define R_X86_64_64 1
#define R_X86_64_GLOB_DAT 6
#define R_X86_64_JUMP_SLOT 7
#define R_X86_64_RELATIVE 8

static int streq(const char *a, const char *b) {
    while (*a && *a == *b) {
        a++;
        b++;
    }
    return *a == *b;
}

/* Resolve an exported symbol `name` inside libc-euro.so (loaded at libc_base). */
static u64 lookup(u64 libc_base, const char *name) {
    unsigned char *eh = (unsigned char *)libc_base;
    u64 phoff = *(u64 *)(eh + 32);
    u16 phnum = *(u16 *)(eh + 56);
    u64 dvaddr = 0;
    for (int i = 0; i < phnum; i++) {
        unsigned char *e = eh + phoff + (u64)i * 56;
        if (*(u32 *)e == 2) { /* PT_DYNAMIC */
            dvaddr = *(u64 *)(e + 16);
            break;
        }
    }
    if (!dvaddr)
        return 0;
    long *d = (long *)(libc_base + dvaddr);
    u64 symtab = 0, strtab = 0, hash = 0;
    for (; d[0] != 0; d += 2) {
        if (d[0] == DT_SYMTAB)
            symtab = libc_base + (u64)d[1];
        else if (d[0] == DT_STRTAB)
            strtab = libc_base + (u64)d[1];
        else if (d[0] == DT_HASH)
            hash = libc_base + (u64)d[1];
    }
    if (!symtab || !strtab || !hash)
        return 0;
    u32 nsym = ((u32 *)hash)[1]; /* nchain == number of symbol-table entries */
    for (u32 i = 0; i < nsym; i++) {
        unsigned char *s = (unsigned char *)(symtab + (u64)i * 24);
        u16 shndx = *(u16 *)(s + 6);
        if (shndx == 0)
            continue; /* undefined */
        char *n = (char *)(strtab + *(u32 *)(s + 0));
        if (streq(n, name))
            return libc_base + *(u64 *)(s + 8); /* st_value */
    }
    return 0;
}

static void reloc(u64 addr, u64 size, u64 exe_base, u64 libc_base, u64 symtab, u64 strtab) {
    for (u64 o = 0; o + 24 <= size; o += 24) {
        u64 *r = (u64 *)(addr + o);
        u64 r_offset = r[0], r_info = r[1];
        long r_addend = (long)r[2];
        u32 type = (u32)(r_info & 0xffffffff);
        u32 sym = (u32)(r_info >> 32);
        u64 *slot = (u64 *)(exe_base + r_offset);
        if (type == R_X86_64_RELATIVE) {
            *slot = exe_base + (u64)r_addend;
        } else if (type == R_X86_64_JUMP_SLOT || type == R_X86_64_GLOB_DAT || type == R_X86_64_64) {
            unsigned char *s = (unsigned char *)(symtab + (u64)sym * 24);
            char *name = (char *)(strtab + *(u32 *)(s + 0));
            u64 v = lookup(libc_base, name);
            *slot = (type == R_X86_64_JUMP_SLOT) ? v : v + (u64)r_addend;
        }
    }
}

/* Called from _start with the initial stack pointer; returns the exe entry. */
u64 ld_main(u64 *sp) {
    u64 argc = sp[0];
    u64 *p = sp + 1 + argc + 1; /* skip argc, argv[], NULL */
    while (*p)
        p++; /* skip envp[] */
    p++;     /* skip envp NULL → auxv */
    u64 at_entry = 0, at_phdr = 0, at_phnum = 0, exe_base = 0, libc_base = 0;
    for (u64 *a = p; a[0] != AT_NULL; a += 2) {
        switch (a[0]) {
        case AT_ENTRY: at_entry = a[1]; break;
        case AT_PHDR: at_phdr = a[1]; break;
        case AT_PHNUM: at_phnum = a[1]; break;
        case AT_EURO_EXE_BASE: exe_base = a[1]; break;
        case AT_EURO_LIBC_BASE: libc_base = a[1]; break;
        }
    }

    /* Find the exe's PT_DYNAMIC via its program headers. */
    unsigned char *ph = (unsigned char *)at_phdr;
    u64 dyn_vaddr = 0;
    for (u64 i = 0; i < at_phnum; i++) {
        unsigned char *e = ph + i * 56;
        if (*(u32 *)e == 2) {
            dyn_vaddr = *(u64 *)(e + 16);
            break;
        }
    }
    long *dyn = (long *)(exe_base + dyn_vaddr);
    u64 symtab = 0, strtab = 0, jmprel = 0, pltrelsz = 0, rela = 0, relasz = 0;
    for (; dyn[0] != 0; dyn += 2) {
        long tag = dyn[0];
        u64 val = (u64)dyn[1];
        if (tag == DT_SYMTAB) symtab = exe_base + val;
        else if (tag == DT_STRTAB) strtab = exe_base + val;
        else if (tag == DT_JMPREL) jmprel = exe_base + val;
        else if (tag == DT_PLTRELSZ) pltrelsz = val;
        else if (tag == DT_RELA) rela = exe_base + val;
        else if (tag == DT_RELASZ) relasz = val;
    }
    /* PLT relocations (JUMP_SLOT) + general relocations (RELATIVE/GLOB_DAT). */
    if (jmprel) reloc(jmprel, pltrelsz, exe_base, libc_base, symtab, strtab);
    if (rela) reloc(rela, relasz, exe_base, libc_base, symtab, strtab);
    return at_entry;
}

/* Entry: save the initial stack, call the linker, then jump to the exe with the
 * original stack (argc/argv/envp/auxv) intact — as if the kernel entered it. */
__asm__(".global _start\n"
        "_start:\n"
        "  mov %rsp, %rbx\n"
        "  and $-16, %rsp\n"
        "  mov %rbx, %rdi\n"
        "  call ld_main\n"
        "  mov %rbx, %rsp\n"
        "  xor %rbp, %rbp\n"
        "  jmp *%rax\n");
