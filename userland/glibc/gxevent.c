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
extern int XCloseDisplay(Display*);
int main(void){
    Display *d = XOpenDisplay(":0");
    if(!d){ printf("GXEVENT: XOpenDisplay NULL\n"); fflush(stdout); _exit(7); }
    Window w = XCreateSimpleWindow(d, XDefaultRootWindow(d), 0,0, 260,180, 0,0, 0x102030);
    XSelectInput(d, w, 0x8000 | 0x1 | 0x4);   /* Exposure|KeyPress|ButtonPress */
    XMapWindow(d, w);
    GC gc = XCreateGC(d, w, 0, 0);
    char ev[256];
    int expose=0, key=0, button=0;
    for(int n=0;n<3;n++){
        XNextEvent(d, ev);
        int type = *(int*)ev;
        if(type==12){ expose=1; XSetForeground(d,gc,0x33cc66); XFillRectangle(d,w,gc,10,10,240,160); XFlush(d); }
        else if(type==2) key=1;
        else if(type==4) button=1;
    }
    printf("GXEVENT: expose=%d key=%d button=%d\n", expose, key, button);
    fflush(stdout);
    for(volatile long i=0;i<20000000;i++) ;
    XCloseDisplay(d);
    _exit((expose && key && button) ? 88 : 1);
}
