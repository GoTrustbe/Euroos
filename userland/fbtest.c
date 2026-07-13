// fbtest — EuroOS app-graphics smoke test.
//
// Proves the userspace framebuffer + key path that the DOOM port will use:
//   * malloc a W*H XRGB8888 buffer in this process's (large) arena,
//   * draw an animated gradient into it,
//   * hand it to the compositor via fb_present(buf, w, h) (syscall 0x6000),
//   * read keys via getkey() (syscall 0x6001) — any key press cycles a colour
//     tint, so an injected keystroke visibly changes the screen.
// Built as a musl static-PIE binary, exactly like muslreal.

#include <stdint.h>
#include <stdlib.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <time.h>

#define SYS_FB_PRESENT 0x6000
#define SYS_GETKEY 0x6001

#define W 320
#define H 200

int main(void) {
    uint32_t *fb = malloc((size_t)W * H * 4);
    if (!fb) {
        return 1;
    }
    unsigned t = 0;
    unsigned tint = 0;
    struct timespec ts = {0, 25 * 1000 * 1000}; // ~25 ms/frame

    for (;;) {
        // Drain key events; a press (bit 8 set) advances the tint 0..3.
        long k;
        while ((k = syscall(SYS_GETKEY)) != 0) {
            if (k & 0x100) {
                tint = (tint + 1) & 3;
            }
        }
        for (int y = 0; y < H; y++) {
            for (int x = 0; x < W; x++) {
                uint8_t r = (uint8_t)(x + t);
                uint8_t g = (uint8_t)(y + t);
                uint8_t b = (uint8_t)(x ^ y);
                if (tint == 1) { g = 0; b = 0; }        // red
                else if (tint == 2) { r = 0; b = 0; }   // green
                else if (tint == 3) { r = 0; g = 0; }   // blue
                fb[y * W + x] = ((uint32_t)r << 16) | ((uint32_t)g << 8) | b;
            }
        }
        syscall(SYS_FB_PRESENT, fb, W, H);
        t += 2;
        syscall(SYS_nanosleep, &ts, (void *)0);
    }
    return 0;
}
