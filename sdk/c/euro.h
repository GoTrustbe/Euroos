/* euro.h: the EuroOS Euro API for freestanding C (no libc, no Linux).
 *
 * Build a native app:  gcc -O2 -ffreestanding -nostdlib -fpie -static-pie \
 *                          -fno-stack-protector -o app app.c
 * Package it:          python3 sdk/euroapp.py build --abi euro --caps console,window ...
 *
 * Every call is one `syscall` instruction (rax = number, rdi/rsi/rdx/r10/r8 =
 * arguments). Results >= 0 are values; negative results are errors (-1 denied
 * by capability, -22 invalid argument, -14 bad pointer, -2 not found, -17 a
 * second surface, -13 outside the file scope). A native app runs under the
 * kernel's `euro` syscall profile: nothing but these calls, exit, memory and
 * sleep exists for it. SPDX-License-Identifier: EUPL-1.2 */
#ifndef EURO_H
#define EURO_H
#include <stdint.h>
#include <stddef.h>

enum {
    EURO_INFO = 0x6100, EURO_SURFACE_CREATE, EURO_SURFACE_PRESENT, EURO_EVENT_POLL, EURO_EVENT_WAIT,
    EURO_TEXT_DRAW, EURO_TEXT_MEASURE, EURO_LOG, EURO_TIME_MS, EURO_SLEEP_MS, EURO_FILE_READ,
    EURO_FILE_WRITE, EURO_EXIT
};
enum { EURO_EV_KEY = 1, EURO_EV_MOVE = 2, EURO_EV_BUTTON = 3, EURO_EV_RESIZE = 4, EURO_EV_CLOSE = 5 };
enum { EURO_CAP_CONSOLE = 1, EURO_CAP_PROC = 2, EURO_CAP_FILE = 4, EURO_CAP_NET = 8, EURO_CAP_WINDOW = 32 };

struct euro_info { uint32_t abi, screen_w, screen_h, caps; };
struct euro_event { uint32_t kind, a, b, c; };

static inline uint64_t euro_syscall(uint64_t n, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5) {
    uint64_t r;
    register uint64_t r10 __asm__("r10") = a4;
    register uint64_t r8 __asm__("r8") = a5;
    __asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a1), "S"(a2), "d"(a3), "r"(r10), "r"(r8) : "rcx", "r11", "memory");
    return r;
}
static inline void euro_info(struct euro_info *i) { euro_syscall(EURO_INFO, (uint64_t)i, sizeof *i, 0, 0, 0); }
static inline int64_t euro_surface_create(uint32_t w, uint32_t h) { return (int64_t)euro_syscall(EURO_SURFACE_CREATE, w, h, 0, 0, 0); }
static inline int64_t euro_surface_present(const uint32_t *px, uint32_t w, uint32_t h) { return (int64_t)euro_syscall(EURO_SURFACE_PRESENT, (uint64_t)px, w, h, 0, 0); }
static inline int euro_event_poll(struct euro_event *e) { return (int)euro_syscall(EURO_EVENT_POLL, (uint64_t)e, sizeof *e, 0, 0, 0); }
static inline int euro_event_wait(uint32_t timeout_ms) { return (int)euro_syscall(EURO_EVENT_WAIT, timeout_ms, 0, 0, 0, 0); }
/* Draw `len` bytes of UTF-8 at (x, y) = top-left of the line, `px` pixels tall, colour 0xRRGGBB. */
static inline int64_t euro_text_draw(uint32_t *buf, uint32_t w, uint32_t h, int32_t x, int32_t y, const char *s, size_t len, uint32_t rgb, uint32_t px) {
    uint64_t dims = ((uint64_t)w << 32) | h, geom = ((uint64_t)(uint32_t)x << 32) | (uint32_t)y;
    uint64_t a5 = ((uint64_t)len << 40) | ((uint64_t)(px & 0xff) << 32) | (rgb & 0xffffff);
    return (int64_t)euro_syscall(EURO_TEXT_DRAW, (uint64_t)buf, dims, geom, (uint64_t)s, a5);
}
static inline uint64_t euro_text_measure(const char *s, size_t len, uint32_t px) { return euro_syscall(EURO_TEXT_MEASURE, (uint64_t)s, len, px, 0, 0); }
static inline int64_t euro_log(const char *s, size_t len) { return (int64_t)euro_syscall(EURO_LOG, (uint64_t)s, len, 0, 0, 0); }
static inline uint64_t euro_time_ms(void) { return euro_syscall(EURO_TIME_MS, 0, 0, 0, 0, 0); }
static inline void euro_sleep_ms(uint32_t ms) { euro_syscall(EURO_SLEEP_MS, ms, 0, 0, 0, 0); }
static inline int64_t euro_file_read(const char *path, size_t plen, void *buf, size_t cap) { return (int64_t)euro_syscall(EURO_FILE_READ, (uint64_t)path, plen, (uint64_t)buf, cap, 0); }
static inline int64_t euro_file_write(const char *path, size_t plen, const void *buf, size_t n) { return (int64_t)euro_syscall(EURO_FILE_WRITE, (uint64_t)path, plen, (uint64_t)buf, n, 0); }
static inline void euro_exit(int code) { for (;;) euro_syscall(EURO_EXIT, (uint64_t)code, 0, 0, 0, 0); }
static inline size_t euro_strlen(const char *s) { size_t n = 0; while (s[n]) n++; return n; }
#endif
