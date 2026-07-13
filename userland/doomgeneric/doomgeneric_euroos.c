// doomgeneric platform port for EuroOS.
//
// Draws each frame to a real compositor window via the EuroOS `fb_present`
// syscall (0x6000) and reads the keyboard via `getkey` (0x6001), which returns
// (pressed<<8 | set-1 scancode). Time comes from clock_gettime; sleep from
// nanosleep. Runs as an unmodified musl static-PIE binary over the Linux ABI.

#include "doomkeys.h"
#include "m_argv.h"
#include "doomgeneric.h"

#include <stdio.h>
#include <unistd.h>
#include <string.h>
#include <stdint.h>
#include <time.h>
#include <sys/syscall.h>

#define SYS_FB_PRESENT 0x6000
#define SYS_GETKEY 0x6001

#define KEYQUEUE_SIZE 16
static unsigned short s_KeyQueue[KEYQUEUE_SIZE];
static unsigned int s_KeyQueueWriteIndex = 0;
static unsigned int s_KeyQueueReadIndex = 0;

// EuroOS delivers raw scancode-set-1 make/break codes; map them to DOOM keys.
static unsigned char convertToDoomKey(unsigned char scancode) {
    switch (scancode) {
    case 0x9C: case 0x1C: return KEY_ENTER;
    case 0x01: return KEY_ESCAPE;
    case 0xCB: case 0x4B: return KEY_LEFTARROW;
    case 0xCD: case 0x4D: return KEY_RIGHTARROW;
    case 0xC8: case 0x48: return KEY_UPARROW;
    case 0xD0: case 0x50: return KEY_DOWNARROW;
    case 0x1D: return KEY_FIRE;      // left ctrl
    case 0x39: return KEY_USE;       // space
    case 0x2A: case 0x36: return KEY_RSHIFT;
    case 0x10: return 'q';
    case 0x15: return 'y';
    default: return 0;
    }
}

static void addKeyToQueue(int pressed, unsigned char keyCode) {
    unsigned char key = convertToDoomKey(keyCode);
    unsigned short keyData = (pressed << 8) | key;
    s_KeyQueue[s_KeyQueueWriteIndex] = keyData;
    s_KeyQueueWriteIndex = (s_KeyQueueWriteIndex + 1) % KEYQUEUE_SIZE;
}

static void handleKeyInput(void) {
    long k;
    while ((k = syscall(SYS_GETKEY)) != 0) {
        int pressed = (k >> 8) & 1;
        unsigned char sc = (unsigned char)(k & 0x7F);
        addKeyToQueue(pressed, sc);
    }
}

void DG_Init(void) {
    // Nothing to set up: fb_present creates/owns the window on first present.
}

void DG_DrawFrame(void) {
    syscall(SYS_FB_PRESENT, DG_ScreenBuffer, DOOMGENERIC_RESX, DOOMGENERIC_RESY);
    handleKeyInput();
}

void DG_SleepMs(uint32_t ms) {
    struct timespec ts;
    ts.tv_sec = ms / 1000;
    ts.tv_nsec = (long)(ms % 1000) * 1000000L;
    syscall(SYS_nanosleep, &ts, (void *)0);
}

uint32_t DG_GetTicksMs(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint32_t)(ts.tv_sec * 1000u + ts.tv_nsec / 1000000u);
}

int DG_GetKey(int *pressed, unsigned char *doomKey) {
    if (s_KeyQueueReadIndex == s_KeyQueueWriteIndex) {
        return 0;
    }
    unsigned short keyData = s_KeyQueue[s_KeyQueueReadIndex];
    s_KeyQueueReadIndex = (s_KeyQueueReadIndex + 1) % KEYQUEUE_SIZE;
    *pressed = keyData >> 8;
    *doomKey = keyData & 0xFF;
    return 1;
}

void DG_SetWindowTitle(const char *title) {
    (void)title;
}

int main(int argc, char **argv) {
    // Unbuffered stdout so DOOM's startup banner + init progress reach the kernel
    // console immediately (musl block-buffers a non-TTY otherwise).
    setvbuf(stdout, NULL, _IONBF, 0);
    doomgeneric_Create(argc, argv);
    for (;;) {
        doomgeneric_Tick();
    }
    return 0;
}
