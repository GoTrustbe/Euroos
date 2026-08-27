/* A persistent, interactive X client: an event loop that redraws forever. Key
   press -> next colour; button press -> move the block. Runs alongside the EuroOS
   desktop, which pumps live keyboard/mouse into it. */
#include <stdio.h>
#include <unistd.h>
typedef struct _XDisplay Display;
typedef unsigned long XID; typedef XID Window; typedef XID Drawable;
typedef struct _XGC* GC;
extern Display* XOpenDisplay(const char*);
extern Window XDefaultRootWindow(Display*);
extern Window XCreateSimpleWindow(Display*, Window, int,int,unsigned,unsigned,unsigned,unsigned long,unsigned long);
extern int XSelectInput(Display*, Window, long);
extern int XMapWindow(Display*, Window);
extern GC XCreateGC(Display*, Drawable, unsigned long, void*);
extern int XSetForeground(Display*, GC, unsigned long);
extern int XFillRectangle(Display*, Drawable, GC, int,int,unsigned,unsigned);
extern int XFlush(Display*);
extern int XNextEvent(Display*, void*);
#define W 400
#define H 280
static unsigned COLORS[5] = {0x3366cc, 0xcc4433, 0x33aa55, 0xddaa22, 0x9955cc};
int main(void){
    Display *d = XOpenDisplay(":0");
    if(!d){ printf("GXLIVE: XOpenDisplay NULL\n"); fflush(stdout); _exit(7); }
    Window w = XCreateSimpleWindow(d, XDefaultRootWindow(d), 0,0, W, H, 0,0, 0x101820);
    XSelectInput(d, w, 0x8000 | 0x1 | 0x4);   /* Exposure|KeyPress|ButtonPress */
    XMapWindow(d, w);
    GC gc = XCreateGC(d, w, 0, 0);
    int ci=0, rx=60, ry=50, keys=0, btns=0;
    char ev[256];
    printf("GXLIVE: live X app running (key=colour, click=move)\n"); fflush(stdout);
    for(;;){
        /* redraw: dark bg + coloured block */
        XSetForeground(d,gc,0x101820); XFillRectangle(d,w,gc,0,0,W,H);
        XSetForeground(d,gc,COLORS[ci]); XFillRectangle(d,w,gc,rx,ry,150,110);
        XFlush(d);
        XNextEvent(d, ev);                     /* block for the next input */
        int t = *(int*)ev;
        if(t==2){ ci=(ci+1)%5; keys++; }
        else if(t==4){ rx+=40; if(rx>W-160) rx=60; ry+=25; if(ry>H-130) ry=50; btns++; }
        if((keys+btns) % 4 == 0){ printf("GXLIVE: keys=%d clicks=%d\n", keys, btns); fflush(stdout); }
    }
}
