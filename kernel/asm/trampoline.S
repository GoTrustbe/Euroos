/* EuroKernel SMP AP-trampoline.
 *
 * Een application-processor (AP) start na een SIPI in 16-bit real mode op
 * physiek 0x8000 (SIPI-vector 0x08). Deze stub brengt de core via protected
 * mode naar long mode (64-bit), met de BSP's boot-PML4 als CR3, en springt naar
 * de Rust-functie `ap_main`. De BSP patcht ap_cr3/ap_stack/ap_entry op vaste
 * offsets (0xF00/0xF08/0xF10) vóór elke SIPI.
 *
 * Geassembleerd door build.rs (`as` + `ld -Ttext=0x8000 --oformat binary`) tot
 * een flat binary die op 0x8000 geladen wordt; absolute labels resolven dus naar
 * 0x8000 + offset.
 */
.code16
.global tramp_start
tramp_start:
    cli
    cld
    xorw %ax, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    movw %ax, %fs
    movw %ax, %gs

    lgdtl gdt32_desc            /* laad de flat-GDT (ds=0, offset < 0x10000) */

    movl %cr0, %eax
    orl  $1, %eax              /* CR0.PE = 1 -> protected mode */
    movl %eax, %cr0
    ljmpl $0x08, $prot32       /* far jump naar 32-bit code-segment */

.code32
prot32:
    movw $0x10, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    movw %ax, %fs
    movw %ax, %gs

    movl %cr4, %eax
    orl  $(1<<5), %eax         /* CR4.PAE = 1 */
    movl %eax, %cr4

    movl ap_cr3, %eax          /* CR3 = boot-PML4 (phys < 4 GiB) */
    movl %eax, %cr3

    movl $0xC0000080, %ecx     /* IA32_EFER */
    rdmsr
    orl  $(1<<8), %eax         /* EFER.LME = 1 */
    wrmsr

    movl %cr0, %eax
    orl  $(1<<31), %eax        /* CR0.PG = 1 -> paging aan (long mode actief) */
    movl %eax, %cr0
    ljmpl $0x18, $long64       /* far jump naar 64-bit code-segment */

.code64
long64:
    movw $0x20, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    movw %ax, %fs
    movw %ax, %gs

    movq ap_stack, %rsp        /* per-AP stack (door BSP gepatcht) */
    movq ap_entry, %rax
    callq *%rax                /* -> ap_main (keert niet terug) */
1:  hlt
    jmp 1b

/* ── Flat GDT: null, code32, data32, code64(L=1), data64 ── */
.align 16
gdt32:
    .quad 0x0000000000000000
    .quad 0x00CF9A000000FFFF   /* 0x08 code32 */
    .quad 0x00CF92000000FFFF   /* 0x10 data32 */
    .quad 0x00AF9A000000FFFF   /* 0x18 code64 (G=1,L=1) */
    .quad 0x00CF92000000FFFF   /* 0x20 data64 */
gdt32_end:
gdt32_desc:
    .word gdt32_end - gdt32 - 1
    .long gdt32                /* base = 0x8000 + offset (door ld geresolved) */

/* ── Door de BSP gepatchte argumenten op vaste offsets ── */
.org 0xF00
.global ap_cr3
ap_cr3:   .quad 0
.global ap_stack
ap_stack: .quad 0
.global ap_entry
ap_entry: .quad 0
