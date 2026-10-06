/* hello-euro-c: a native EuroOS window in C, no libc, no X11.
 * Build: gcc -O2 -ffreestanding -nostdlib -fpie -static-pie -fno-stack-protector \
 *            -fno-asynchronous-unwind-tables -o hello-euro-c hello-euro-c.c */
#include "euro.h"

#define W 480
#define H 240
static uint32_t px[W * H];

static void rect(int x, int y, int w, int h, uint32_t c) {
    for (int yy = y; yy < y + h && yy < H; yy++)
        for (int xx = x; xx < x + w && xx < W; xx++)
            if (xx >= 0 && yy >= 0) px[yy * W + xx] = c;
}

void _start(void) {
    struct euro_info info;
    euro_info(&info);
    euro_log("hello-euro-c: starting", 22);
    if (!(info.caps & EURO_CAP_WINDOW) || euro_surface_create(W, H) < 0) euro_exit(2);
    rect(0, 0, W, H, 0xffffff);
    rect(0, 0, W, 56, 0x2d6be0);
    const char *t1 = "Hello from C", *t2 = "A native EuroOS app in freestanding C: no libc, no X11.", *t3 = "Press Esc to quit.";
    euro_text_draw(px, W, H, 18, 14, t1, euro_strlen(t1), 0xffffff, 24);
    euro_text_draw(px, W, H, 18, 80, t2, euro_strlen(t2), 0x141821, 16);
    euro_text_draw(px, W, H, 18, 120, t3, euro_strlen(t3), 0x55595f, 15);
    euro_surface_present(px, W, H);
    for (;;) {
        if (!euro_event_wait(500)) continue;
        struct euro_event e;
        while (euro_event_poll(&e)) {
            if (e.kind == EURO_EV_CLOSE || (e.kind == EURO_EV_KEY && e.b && e.a == 0x01)) euro_exit(0);
        }
    }
}
