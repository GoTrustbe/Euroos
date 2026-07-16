/* Combined X11 client: connect + window + fill + PutImage + events + real keyboard.
   One process (one library load) exercising the whole X path. */
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
extern Window XCreateSimpleWindow(Display*, Window, int,int,unsigned,unsigned,unsigned,unsigned long,unsigned long);
extern int XSelectInput(Display*, Window, long);
extern int XMapWindow(Display*, Window);
extern GC XCreateGC(Display*, Drawable, unsigned long, void*);
extern int XSetForeground(Display*, GC, unsigned long);
extern int XFillRectangle(Display*, Drawable, GC, int,int,unsigned,unsigned);
extern XImage* XCreateImage(Display*, Visual*, unsigned, int, int, char*, unsigned, unsigned, int, int);
extern int XPutImage(Display*, Drawable, GC, XImage*, int,int,int,int, unsigned, unsigned);
extern int XFlush(Display*);
extern int XPending(Display*);
extern int XNextEvent(Display*, void*);
extern int XCloseDisplay(Display*);
#define IW 120
#define IH 80
int main(void){
    Display *d = XOpenDisplay(":0");
    if(!d){ printf("GXWIN: XOpenDisplay NULL\n"); fflush(stdout); _exit(7); }
    int s = XDefaultScreen(d);
    Window w = XCreateSimpleWindow(d, XDefaultRootWindow(d), 0,0, 300,200, 0,0, 0x181818);
    XSelectInput(d, w, 0x8000 | 0x1);         /* Exposure | KeyPress */
    XMapWindow(d, w);
    GC gc = XCreateGC(d, w, 0, 0);
    XSetForeground(d, gc, 0x3366cc);
    XFillRectangle(d, w, gc, 20, 20, 260, 100);   /* solid fill */
    /* PutImage: a small 4-colour-quadrant raster below the fill */
    unsigned *px = malloc(IW*IH*4);
    for(int y=0;y<IH;y++) for(int x=0;x<IW;x++){
        int top=y<IH/2, left=x<IW/2;
        px[y*IW+x] = top&&left?0xff0000:top?0x00ff00:left?0x0000ff:0xffffff;
    }
    XImage *img = XCreateImage(d, XDefaultVisual(d,s), XDefaultDepth(d,s), 2, 0, (char*)px, IW, IH, 32, 0);
    XPutImage(d, w, gc, img, 0,0, 90,130, IW, IH);
    XFlush(d);
    printf("GXWIN: window mapped, filled, PutImage done; waiting for events...\n"); fflush(stdout);
    int expose=0, keys=0, tries=0; char ev[256];
    while((!expose || keys<3) && tries<400){
        if(XPending(d) > 0){
            XNextEvent(d, ev);
            int t = *(int*)ev;
            if(t==12) expose=1; else if(t==2) keys++;
        } else { for(volatile int i=0;i<150000;i++) ; tries++; }
    }
    printf("GXWIN: connect=1 render+putimage=1 expose=%d keys=%d -> %s\n",
           expose, keys, (expose && keys>=3) ? "PASS":"PARTIAL");
    fflush(stdout);
    for(volatile long i=0;i<20000000;i++) ;   /* hold for a screenshot */
    XCloseDisplay(d);
    _exit((expose && keys>=3) ? 90 : 1);
}
