/* gpcm: [pcmch] (BUG-181) — the ALSA playback device with 6, 1 and 2 channels.
 * Chromium opens hw:0,0 with the channel count of the media (6 for 5.1 Opus) and
 * does not downmix when the device is named; the device must take 1..8 channels
 * as a real HD-Audio codec driver with a channel map does, and play them as the
 * stereo the hardware has. For each count this does what libasound's
 * snd_pcm_set_params does with raw ioctls: HW_REFINE of the whole space (the
 * count must be in the answer), HW_REFINE with the count, rate, period and
 * buffer fixed, HW_PARAMS, PREPARE, one WRITEI of a known pattern (all of it
 * must be taken), DROP, HW_FREE. The kernel checks what reached the ring.
 * Every step is one line "ok"/"FAIL"; exit 77 = all passed, 78 = no device. */
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#define HW_REFINE 0xc2604110UL
#define HW_PARAMS 0xc2604111UL
#define HW_FREE   0x4112UL
#define PREPARE   0x4140UL
#define DROP      0x4143UL
#define WRITEI    0x40184150UL

static int passed, checks;
static void check(int ok, const char *what, long v) {
    checks++;
    if (ok) passed++;
    printf("%s %s (%ld)\n", ok ? "ok  " : "FAIL", what, v);
}

static uint8_t p[608];
static void ival(int param, uint32_t lo, uint32_t hi) {
    uint8_t *o = p + 260 + (param - 8) * 12;
    memcpy(o, &lo, 4); memcpy(o + 4, &hi, 4);
    uint32_t f = 0; memcpy(o + 8, &f, 4);
}
static void gval(int param, uint32_t *lo, uint32_t *hi) {
    uint8_t *o = p + 260 + (param - 8) * 12;
    memcpy(lo, o, 4); memcpy(hi, o + 4, 4);
}
static void any(void) {
    memset(p, 0, sizeof p);
    memset(p + 4, 0xff, 3 * 32);
    for (int i = 8; i <= 19; i++) ival(i, 0, 0xffffffffu);
    uint32_t r = 0xffffffffu; memcpy(p + 512, &r, 4);
}
static void mask1(int idx, int bit) {
    memset(p + 4 + idx * 32, 0, 32);
    uint32_t v = 1u << bit; memcpy(p + 4 + idx * 32, &v, 4);
}

/* channel k of every frame: the value the kernel's check expects */
static int16_t sample(int ch, int k) {
    if (ch == 1) return 5000;
    if (ch == 2) return k == 0 ? 1234 : -4321;
    static const int16_t six[6] = {12000, -12000, 6000, -6000, 4000, 30000}; /* FL FR RL RR FC LFE */
    return six[k];
}

static int stream(int fd, int ch) {
    char w[160];
    any();
    int r = ioctl(fd, HW_REFINE, p);
    uint32_t lo, hi; gval(10, &lo, &hi);
    snprintf(w, sizeof w, "%d ch: the whole space offers channels [%u,%u]", ch, lo, hi);
    check(r == 0 && lo <= (uint32_t)ch && (uint32_t)ch <= hi, w, r ? -errno : 0);
    any();
    mask1(0, 3); mask1(1, 2); mask1(2, 0);       /* RW_INTERLEAVED, S16_LE, STD */
    ival(10, ch, ch); ival(11, 48000, 48000);    /* CHANNELS, RATE */
    ival(13, 1024, 1024); ival(17, 8192, 8192);  /* PERIOD_SIZE, BUFFER_SIZE */
    r = ioctl(fd, HW_REFINE, p);
    uint32_t fb0, fb1; gval(9, &fb0, &fb1); gval(10, &lo, &hi);
    snprintf(w, sizeof w, "%d ch: refine with the count: channels [%u,%u], frame bits [%u,%u]", ch, lo, hi, fb0, fb1);
    check(r == 0 && lo == (uint32_t)ch && hi == (uint32_t)ch && fb0 == 16u * ch && fb1 == 16u * ch, w, r ? -errno : 0);
    r = ioctl(fd, HW_PARAMS, p);
    snprintf(w, sizeof w, "%d ch: HW_PARAMS", ch);
    check(r == 0, w, r ? -errno : 0);
    if (r) return -1;
    r = ioctl(fd, PREPARE);
    check(r == 0, "PREPARE", r ? -errno : 0);
    static int16_t buf[256 * 8];
    for (int f = 0; f < 256; f++)
        for (int k = 0; k < ch; k++) buf[f * ch + k] = sample(ch, k);
    struct { long result; void *buf; unsigned long frames; } x = { 0, buf, 256 };
    r = ioctl(fd, WRITEI, &x);
    snprintf(w, sizeof w, "%d ch: WRITEI of 256 frames takes 256", ch);
    check(r == 0 && x.result == 256, w, r ? -errno : x.result);
    ioctl(fd, DROP);
    ioctl(fd, HW_FREE);
    return 0;
}

int main(void) {
    int fd = open("/dev/snd/pcmC0D0p", O_RDWR);
    if (fd < 0) {
        printf("GPCM: no playback device (errno %d): not run -> SKIP\n", errno);
        return 78;
    }
    int order[3] = {6, 1, 2};
    for (int i = 0; i < 3; i++) {
        stream(fd, order[i]);
        /* the kernel reads the ring between streams: say which one ended */
        printf("GPCM-STREAM %d\n", order[i]);
        fflush(stdout);
    }
    close(fd);
    int ok = checks > 0 && passed == checks;
    printf("GPCM: %d of %d -> %s\n", passed, checks, ok ? "PASS" : "FAILED");
    return ok ? 77 : 1;
}
