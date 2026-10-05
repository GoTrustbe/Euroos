/* hello-x11: a windowed EuroOS app. Opens an X11 window on the in-kernel X
   server (DISPLAY=:0), draws a coloured panel with text, and quits on a key or
   after the window is closed. Xlib only: libX11 is part of the library set
   EuroOS serves. Build: gcc -O2 -o hello hello.c -lX11 */
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <stdio.h>
#include <string.h>

int main(void) {
    Display *d = XOpenDisplay(NULL);
    if (!d) { fprintf(stderr, "hello-x11: cannot open DISPLAY\n"); return 1; }
    int s = DefaultScreen(d);
    Window w = XCreateSimpleWindow(d, RootWindow(d, s), 60, 60, 480, 240, 1,
                                   BlackPixel(d, s), WhitePixel(d, s));
    XStoreName(d, w, "Hello from a EuroApp");
    XSelectInput(d, w, ExposureMask | KeyPressMask | ButtonPressMask);
    XMapWindow(d, w);
    GC gc = XCreateGC(d, w, 0, NULL);
    const char *l1 = "Hello, EuroOS!";
    const char *l2 = "This window is a third-party app: packaged, signed, installed with eupkg.";
    const char *l3 = "Press any key to quit.";
    for (;;) {
        XEvent e;
        XNextEvent(d, &e);
        if (e.type == Expose) {
            XSetForeground(d, gc, 0x1E4FB8);
            XFillRectangle(d, w, gc, 0, 0, 480, 60);
            XSetForeground(d, gc, WhitePixel(d, s));
            XDrawString(d, w, gc, 20, 38, l1, (int)strlen(l1));
            XSetForeground(d, gc, BlackPixel(d, s));
            XDrawString(d, w, gc, 20, 110, l2, (int)strlen(l2));
            XDrawString(d, w, gc, 20, 150, l3, (int)strlen(l3));
        }
        if (e.type == KeyPress || e.type == ButtonPress) break;
    }
    XDestroyWindow(d, w);
    XCloseDisplay(d);
    return 0;
}
