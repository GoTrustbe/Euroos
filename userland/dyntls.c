/* Roept bump() aan uit libtls.so (JUMP_SLOT) → de .so leest zijn __thread `ctr`
 * via %fs (TPOFF64). 41 -> 42 -> exit(42). */
extern long bump(void);
static void sys_exit(long c){ __asm__ volatile("syscall"::"a"(60),"D"(c):"rcx","r11","memory"); }
void _start(void){ sys_exit(bump()); __builtin_unreachable(); }
