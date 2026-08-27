#include <stdio.h>
#include <unistd.h>
typedef struct _XDisplay Display;
typedef unsigned long XID; typedef XID Window; typedef XID Drawable;
typedef struct _XGC* GC;
extern Display* XOpenDisplay(const char*);
extern Window XDefaultRootWindow(Display*);
extern Window XCreateSimpleWindow(Display*, Window, int, int, unsigned, unsigned, unsigned, unsigned long, unsigned long);
extern int XMapWindow(Display*, Window);
extern GC XCreateGC(Display*, Drawable, unsigned long, void*);
extern int XSetForeground(Display*, GC, unsigned long);
extern int XFillRectangle(Display*, Drawable, GC, int, int, unsigned, unsigned);
extern int XFlush(Display*);
extern int XSync(Display*, int);
extern int XCloseDisplay(Display*);
int main(void){
    Display *d = XOpenDisplay(":0");
    if(!d){ printf("GXDRAW: XOpenDisplay NULL\n"); fflush(stdout); _exit(7); }
    Window root = XDefaultRootWindow(d);
    Window w = XCreateSimpleWindow(d, root, 0, 0, 300, 200, 0, 0, 0x202020);
    XMapWindow(d, w);
    GC gc = XCreateGC(d, w, 0, 0);
    XSetForeground(d, gc, 0x3366cc);
    XFillRectangle(d, w, gc, 20, 20, 260, 160);
    XFlush(d);
    XSync(d, 0);                 /* roundtrip: server has processed the fill */
    printf("GXDRAW: 300x200 window mapped + filled 0x3366cc\n");
    fflush(stdout);
    for(volatile long i=0;i<20000000;i++) ; /* hold so the frame is visible */
    XCloseDisplay(d);
    _exit(0);
}
