// EuroBrowser — a real userspace web browser for EuroOS.
//
// A separate musl process (like the DOOM port), it fetches LIVE websites over
// the kernel's HTTP/1.1 + TLS 1.3 + DNS stack (via the fetch_start/fetch_poll
// syscalls), strips HTML to readable text, keeps the links, and renders to the
// framebuffer with its own 8x8 font. Navigate with the URL bar (type + Enter),
// click links with the mouse, scroll with the arrows/PageUp/Down, Left arrow to
// go back, Esc to quit. Honest scope: a text-mode browser (no CSS/JS/images),
// but a genuine, operable browser process on real sites.

#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <stdlib.h>

#include "font8x8_basic.h"

// ---- raw syscalls (Linux x86-64 convention) ----
static long syscall3(long n, long a, long b, long c) {
    long r;
    __asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c)
                     : "rcx", "r11", "memory");
    return r;
}
#define SYS_write     1
#define SYS_exit      60
#define FB_PRESENT    0x6000
#define GETKEY        0x6001
#define FETCH_START   0x6002
#define FETCH_POLL    0x6003
#define GET_MOUSE     0x6004
#define GET_SCREEN    0x6005

static void present(uint32_t *buf, int w, int h) { syscall3(FB_PRESENT, (long)buf, w, h); }
static int  getkey(void) { return (int)syscall3(GETKEY, 0, 0, 0); }
static void fetch_start(const char *url, int len) { syscall3(FETCH_START, (long)url, len, 0); }
static unsigned long fetch_poll(char *out, int cap) { return (unsigned long)syscall3(FETCH_POLL, (long)out, cap, 0); }
static void get_mouse(int *out12) { syscall3(GET_MOUSE, (long)out12, 0, 0); }
static void get_screen(int *out8) { syscall3(GET_SCREEN, (long)out8, 0, 0); }
static void logs(const char *s) { syscall3(SYS_write, 2, (long)s, (long)strlen(s)); }
// Yield the CPU (~20ms) so the desktop-loop task can run the network fetch and
// service input. Without this a busy-loop starves the fetch.
static void nap(void) { long ts[2] = {0, 5000000}; syscall3(35 /*nanosleep*/, (long)ts, 0, 0); }
static void die(int c) { syscall3(SYS_exit, c, 0, 0); for(;;){} }

// ---- framebuffer + font ----
static uint32_t *FB;
static int SW, SH;
#define BG      0x00101826u
#define FG      0x00E6ECF5u
#define BARBG   0x001d2b45u
#define LINKC   0x005e8af2u
#define ACCENT  0x002d6be0u

static void fill(int x, int y, int w, int h, uint32_t c) {
    if (x < 0) { w += x; x = 0; }
    if (y < 0) { h += y; y = 0; }
    if (x + w > SW) w = SW - x;
    if (y + h > SH) h = SH - y;
    for (int j = 0; j < h; j++) {
        uint32_t *row = FB + (long)(y + j) * SW + x;
        for (int i = 0; i < w; i++) row[i] = c;
    }
}
// Draw one 8x8 glyph at scale s.
static void glyph(int x, int y, char ch, uint32_t c, int s) {
    unsigned uc = (unsigned char)ch;
    if (uc >= 128) uc = '?';
    const char *g = font8x8_basic[uc];
    for (int row = 0; row < 8; row++) {
        for (int col = 0; col < 8; col++) {
            if (g[row] & (1 << col)) fill(x + col * s, y + row * s, s, s, c);
        }
    }
}
static int CW, CH; // cell width/height (scaled glyph advance)
static void text(int x, int y, const char *s, uint32_t c) {
    for (; *s; s++) { glyph(x, y, *s, c, CH / 8); x += CW; }
}

// ---- page model ----
#define MAXHTML  262144
#define MAXWORDS 30000
#define MAXHREF  4000
#define MAXURL   512

static char *html;                 // fetched body
static char *textbuf;              // stripped text pool
static char **words;               // pointers into textbuf ("\n" = break)
static int   *wordlink;            // href index per word, or -1
static char **hrefs;               // link targets
static int nwords, nhref;

static char cur_url[MAXURL] = "http://euro-os.eu/";
static char url_edit[MAXURL];      // the editable URL bar
static int  url_len;
static char status[128] = "Ready";
static int scroll;                 // first visible line
static char history[32][MAXURL];   // simple back stack
static int histn;

// Click regions computed during render (word rects that are links).
struct Rect { int x, y, w, h, href; };
static struct Rect *clicks;
static int nclicks;

static int str_ieq(const char *a, const char *b, int n) {
    for (int i = 0; i < n; i++) {
        char ca = a[i], cb = b[i];
        if (ca >= 'A' && ca <= 'Z') ca += 32;
        if (cb >= 'A' && cb <= 'Z') cb += 32;
        if (ca != cb) return 0;
    }
    return 1;
}

// Decode a handful of HTML entities in place-ish (append to out).
static int put_entity(const char *e, int elen, char *out) {
    if (elen == 4 && str_ieq(e, "amp;", 4)) { out[0] = '&'; return 1; }
    if (elen == 3 && str_ieq(e, "lt;", 3))  { out[0] = '<'; return 1; }
    if (elen == 3 && str_ieq(e, "gt;", 3))  { out[0] = '>'; return 1; }
    if (elen == 5 && str_ieq(e, "quot;", 5)){ out[0] = '"'; return 1; }
    if (elen == 6 && str_ieq(e, "nbsp;", 5)){ out[0] = ' '; return 1; }
    if (elen >= 5 && str_ieq(e, "#39;", 4)) { out[0] = '\''; return 1; }
    return 0;
}

// Parse HTML → words[] (+ "\n" breaks) and hrefs[]. Skips script/style.
static void parse(void) {
    nwords = nhref = 0;
    int tlen = 0;
    int cur_href = -1;       // active <a> link index
    int in_word = 0;
    char *p = html;
    while (*p && nwords < MAXWORDS - 2) {
        if (*p == '<') {
            // Tag. Read the tag name.
            char *t = p + 1;
            int close = (*t == '/');
            if (close) t++;
            char name[16]; int nl = 0;
            while (*t && *t != '>' && *t != ' ' && *t != '\t' && *t != '\n' && nl < 15) name[nl++] = *t++;
            name[nl] = 0;
            // Skip <script>/<style> bodies entirely.
            if (!close && (str_ieq(name, "script", 6) || str_ieq(name, "style", 5))) {
                char end[10];
                int el = 0; end[el++] = '<'; end[el++] = '/';
                for (int i = 0; i < nl; i++) end[el++] = name[i];
                end[el] = 0;
                char *e = strstr(p + 1, end);
                p = e ? e : (p + strlen(p));
                continue;
            }
            // Anchor: capture href on open, close the link on </a>.
            if (str_ieq(name, "a", 1) && nl == 1) {
                if (close) { cur_href = -1; }
                else {
                    char *h = p;
                    char *lim = strchr(p, '>');
                    if (!lim) lim = p + strlen(p);
                    char *hp = NULL;
                    for (char *s = h; s < lim - 4; s++) {
                        if (str_ieq(s, "href", 4)) { hp = s + 4; break; }
                    }
                    if (hp && nhref < MAXHREF) {
                        while (*hp == ' ' || *hp == '=' ) hp++;
                        char q = 0;
                        if (*hp == '"' || *hp == '\'') q = *hp++;
                        char *hv = hrefs[nhref];
                        int hl = 0;
                        while (*hp && hl < MAXURL - 1) {
                            if (q && *hp == q) break;
                            if (!q && (*hp == ' ' || *hp == '>')) break;
                            hv[hl++] = *hp++;
                        }
                        hv[hl] = 0;
                        cur_href = nhref++;
                    }
                }
            }
            // Block tags force a line break.
            if (str_ieq(name, "br", 2) || str_ieq(name, "p", 1) || str_ieq(name, "div", 3) ||
                str_ieq(name, "li", 2) || str_ieq(name, "tr", 2) || str_ieq(name, "h1", 2) ||
                str_ieq(name, "h2", 2) || str_ieq(name, "h3", 2) || str_ieq(name, "h4", 2) ||
                str_ieq(name, "ul", 2) || str_ieq(name, "ol", 2) || str_ieq(name, "table", 5)) {
                if (in_word) { textbuf[tlen++] = 0; in_word = 0; }
                words[nwords] = "\n"; wordlink[nwords] = -1; nwords++;
            }
            char *gt = strchr(p, '>');
            p = gt ? gt + 1 : p + strlen(p);
            continue;
        }
        // Text run.
        char c = *p;
        if (c == '&') {
            char decoded;
            if (put_entity(p + 1, (int)strlen(p + 1) > 6 ? 6 : (int)strlen(p + 1), &decoded)) {
                // advance past the entity
                char *semi = strchr(p, ';');
                p = semi ? semi + 1 : p + 1;
                if (!in_word) { words[nwords] = &textbuf[tlen]; wordlink[nwords] = cur_href; nwords++; in_word = 1; }
                textbuf[tlen++] = decoded;
                continue;
            }
        }
        if (c == ' ' || c == '\t' || c == '\n' || c == '\r') {
            if (in_word) { textbuf[tlen++] = 0; in_word = 0; }
            p++;
            continue;
        }
        if (!in_word) { words[nwords] = &textbuf[tlen]; wordlink[nwords] = cur_href; nwords++; in_word = 1; }
        textbuf[tlen++] = c;
        p++;
        if (tlen > MAXHTML * 2 - 8) break;
    }
    if (in_word) textbuf[tlen++] = 0;
    scroll = 0;
}

// ---- URL resolution (relative → absolute against cur_url) ----
static void resolve(const char *href, char *out) {
    if (!strncmp(href, "http://", 7) || !strncmp(href, "https://", 8)) {
        strncpy(out, href, MAXURL - 1); out[MAXURL-1]=0; return;
    }
    // scheme + host from cur_url
    char scheme_host[MAXURL]; int shl = 0;
    const char *pp = cur_url;
    const char *slashes = strstr(cur_url, "://");
    if (slashes) {
        const char *hostend = strchr(slashes + 3, '/');
        int n = hostend ? (int)(hostend - cur_url) : (int)strlen(cur_url);
        for (int i = 0; i < n && shl < MAXURL-1; i++) scheme_host[shl++] = cur_url[i];
    }
    scheme_host[shl] = 0;
    (void)pp;
    if (href[0] == '/') {
        strncpy(out, scheme_host, MAXURL-1);
        strncat(out, href, MAXURL-1-strlen(out));
    } else {
        // relative to the current directory
        char base[MAXURL]; strncpy(base, cur_url, MAXURL-1); base[MAXURL-1]=0;
        char *lastslash = strrchr(base + (slashes ? (slashes - cur_url + 3) : 0), '/');
        if (lastslash) lastslash[1] = 0;
        strncpy(out, base, MAXURL-1); out[MAXURL-1]=0;
        strncat(out, href, MAXURL-1-strlen(out));
    }
}

// ---- fetch ----
static int fetching;
static void navigate(const char *url, int push) {
    if (push && histn < 32) { strncpy(history[histn], cur_url, MAXURL-1); history[histn][MAXURL-1]=0; histn++; }
    strncpy(cur_url, url, MAXURL-1); cur_url[MAXURL-1] = 0;
    strncpy(url_edit, cur_url, MAXURL-1); url_len = strlen(url_edit);
    logs("browser: fetching "); logs(cur_url); logs("\n");
    strcpy(status, "Loading...");
    fetch_start(cur_url, strlen(cur_url));
    fetching = 1;
}

static void num_to_str(unsigned long n, char *out) {
    char tmp[24]; int i = 0;
    if (n == 0) { out[0]='0'; out[1]=0; return; }
    while (n && i < 23) { tmp[i++] = '0' + (n % 10); n /= 10; }
    int j = 0; while (i) out[j++] = tmp[--i]; out[j] = 0;
}

// ---- render ----
#define TOPBAR 40
static void render(void) {
    fill(0, 0, SW, SH, BG);
    // URL bar.
    fill(0, 0, SW, TOPBAR, BARBG);
    text(10, 12, "URL:", ACCENT + 0x00303030);
    char shown[MAXURL + 2];
    strncpy(shown, url_edit, MAXURL); shown[MAXURL]=0;
    int sl = strlen(shown); if (sl < MAXURL) { shown[sl] = '_'; shown[sl+1] = 0; }
    text(10 + 5 * CW, 12, shown, FG);
    // Status (right side of the bar).
    int stx = SW - (int)strlen(status) * CW - 12;
    text(stx, 12, status, 0x009FB0C8);

    // Page body.
    nclicks = 0;
    int x = 12, y = TOPBAR + 8 - scroll * CH;
    int line = 0;
    int wrap = SW - 24;
    for (int i = 0; i < nwords; i++) {
        if (strcmp(words[i], "\n") == 0) { x = 12; y += CH; line++; continue; }
        int wpx = strlen(words[i]) * CW + CW; // + a space
        if (x + wpx > wrap) { x = 12; y += CH; line++; }
        if (y > TOPBAR && y < SH) {
            int link = wordlink[i];
            uint32_t c = (link >= 0) ? LINKC : FG;
            text(x, y, words[i], c);
            if (link >= 0) {
                int w = strlen(words[i]) * CW;
                fill(x, y + CH - 2, w, 1, LINKC);
                if (nclicks < MAXWORDS) {
                    clicks[nclicks].x = x; clicks[nclicks].y = y; clicks[nclicks].w = w;
                    clicks[nclicks].h = CH; clicks[nclicks].href = link; nclicks++;
                }
            }
        }
        x += wpx;
        if (y > SH + CH) break;
    }
    present(FB, SW, SH);
}

// ---- scancode → ascii (set 1, unshifted) ----
static const char SC[128] = {
 0,27,'1','2','3','4','5','6','7','8','9','0','-','=','\b','\t',
 'q','w','e','r','t','y','u','i','o','p','[',']','\n',0,'a','s',
 'd','f','g','h','j','k','l',';','\'','`',0,'\\','z','x','c','v',
 'b','n','m',',','.','/',0,'*',0,' ',0,0,0,0,0,0,
 0,0,0,0,0,0,0,'7','8','9','-','4','5','6','+','1',
 '2','3','0','.',0,0,0,0,0,0,0,0,0,0,0,0 };

int main(void) {
    int scr[2]; get_screen(scr);
    SW = scr[0]; SH = scr[1];
    if (SW <= 0 || SH <= 0) { SW = 1024; SH = 768; }
    CH = 16; CW = 16; // 2x scale of the 8x8 font (advance = 16)
    FB = malloc((long)SW * SH * 4);
    html = malloc(MAXHTML + 8);
    textbuf = malloc(MAXHTML * 2 + 16);
    words = malloc(sizeof(char*) * MAXWORDS);
    wordlink = malloc(sizeof(int) * MAXWORDS);
    hrefs = malloc(sizeof(char*) * MAXHREF);
    clicks = malloc(sizeof(struct Rect) * MAXWORDS);
    for (int i = 0; i < MAXHREF; i++) hrefs[i] = malloc(MAXURL);
    if (!FB || !html || !textbuf || !words || !wordlink || !hrefs || !clicks) { logs("browser: OOM\n"); die(1); }

    logs("browser: started, screen ");
    { char b[16]; num_to_str(SW, b); logs(b); logs("x"); num_to_str(SH, b); logs(b); logs("\n"); }

    nwords = 0;
    navigate(cur_url, 0);
    render();

    int last_btn = 0;
    int need_render = 1;
    int warmup = 60; // present a few frames at launch to win the screen from the desktop
    for (;;) {
        // Poll a pending fetch result.
        if (fetching) {
            unsigned long r = fetch_poll(html, MAXHTML);
            if (r != (unsigned long)-1) {
                int stcode = (int)(r >> 32);
                int len = (int)(r & 0xFFFFFFFF);
                html[len < MAXHTML ? len : MAXHTML - 1] = 0;
                fetching = 0;
                char b[16]; num_to_str(len, b);
                logs("browser: got status "); { char s2[16]; num_to_str(stcode, s2); logs(s2); }
                logs(", "); logs(b); logs(" bytes\n");
                if (len == 0) strcpy(status, "Load failed");
                else { char s3[24]; num_to_str(stcode, s3); strcpy(status, "HTTP "); strcat(status, s3); parse(); }
                need_render = 1;
            }
        }
        // Keyboard.
        int k;
        while ((k = getkey()) != 0) {
            int pressed = (k >> 8) & 1;
            int sc = k & 0xFF;
            if (!pressed) continue;
            if (sc == 0x01) { logs("browser: quit\n"); die(0); }        // Esc
            else if (sc == 0x1C) {                                       // Enter → navigate
                url_edit[url_len] = 0; navigate(url_edit, 1); need_render = 1;
            }
            else if (sc == 0x0E) { if (url_len > 0) url_len--; url_edit[url_len]=0; need_render = 1; } // Backspace
            else if (sc == 0x48) { if (scroll > 0) scroll--; need_render = 1; }        // Up
            else if (sc == 0x50) { scroll++; need_render = 1; }                        // Down
            else if (sc == 0x49) { scroll -= 20; if (scroll < 0) scroll = 0; need_render = 1; } // PgUp
            else if (sc == 0x51) { scroll += 20; need_render = 1; }                    // PgDn
            else if (sc == 0x47) { scroll = 0; need_render = 1; }                       // Home
            else if (sc == 0x4B) {                                                      // Left → back
                if (histn > 0) { histn--; navigate(history[histn], 0); need_render = 1; }
            }
            else {
                char ch = (sc < 128) ? SC[sc] : 0;
                if (ch >= 32 && ch < 127 && url_len < MAXURL - 2) { url_edit[url_len++] = ch; url_edit[url_len]=0; need_render = 1; }
            }
        }
        // Mouse: click a link.
        int m[3]; get_mouse(m);
        int btn = m[2] & 1;
        if (btn && !last_btn) {
            int mx = m[0], my = m[1];
            for (int i = 0; i < nclicks; i++) {
                struct Rect *r = &clicks[i];
                if (mx >= r->x && mx < r->x + r->w && my >= r->y && my < r->y + r->h) {
                    char abs[MAXURL]; resolve(hrefs[r->href], abs);
                    navigate(abs, 1); need_render = 1; break;
                }
            }
        }
        last_btn = btn;

        // Present every iteration so the browser keeps ownership of the screen
        // (an event-driven app can lose it to a desktop repaint race, unlike a
        // 60fps game). need_render still gates the expensive re-layout.
        // Present a burst at launch to take the screen from the desktop, then
        // only on change. During the fetch we busy-poll WITHOUT the expensive
        // full-screen blit, so the desktop-loop task gets the CPU to fetch fast.
        if (need_render || warmup > 0) {
            render();
            if (need_render) need_render = 0;
            if (warmup > 0) warmup--;
        }
    }
    return 0;
}
