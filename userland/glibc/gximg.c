#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
typedef struct _XDisplay Display;
typedef unsigned long XID; typedef XID Window; typedef XID Drawable;
typedef struct _XGC* GC; typedef struct _Visual Visual; typedef struct _XImage XImage;
extern Display* XOpenDisplay(const char*);
extern Window XDefaultRootWindow(Display*);
extern int XDefaultScreen(Display*);
extern Visual* XDefaultVisual(Display*, int);
extern int XDefaultDepth(Display*, int);
extern Window XCreateSimpleWindow(Display*, Window, int, int, unsigned, unsigned, unsigned, unsigned long, unsigned long);
extern int XMapWindow(Display*, Window);
extern GC XCreateGC(Display*, Drawable, unsigned long, void*);
extern XImage* XCreateImage(Display*, Visual*, unsigned, int, int, char*, unsigned, unsigned, int, int);
extern int XPutImage(Display*, Drawable, GC, XImage*, int,int,int,int, unsigned, unsigned);
extern int XFlush(Display*);
extern int XSync(Display*, int);
extern int XCloseDisplay(Display*);
#define W 240
#define H 160
int main(void){
    Display *d = XOpenDisplay(":0");
    if(!d){ printf("GXIMG: XOpenDisplay NULL\n"); fflush(stdout); _exit(7); }
    int s = XDefaultScreen(d);
    Window w = XCreateSimpleWindow(d, XDefaultRootWindow(d), 0,0, W, H, 0, 0, 0x101010);
    XMapWindow(d, w);
    GC gc = XCreateGC(d, w, 0, 0);
    unsigned *px = malloc(W*H*4);
    for(int y=0;y<H;y++) for(int x=0;x<W;x++){
        int top=y<H/2, left=x<W/2;
        px[y*W+x] = top&&left ? 0xff0000 : top ? 0x00ff00 : left ? 0x0000ff : 0xffffff;
    }
    XImage *img = XCreateImage(d, XDefaultVisual(d,s), XDefaultDepth(d,s), 2, 0, (char*)px, W, H, 32, 0);
    XPutImage(d, w, gc, img, 0,0, 0,0, W, H);
    XFlush(d); XSync(d, 0);
    printf("GXIMG: %dx%d ZPixmap put (red/green/blue/white quadrants)\n", W, H);
    fflush(stdout);
    for(volatile long i=0;i<20000000;i++) ;
    XCloseDisplay(d);
    _exit(0);
}
