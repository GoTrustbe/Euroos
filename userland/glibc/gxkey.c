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
extern int XPending(Display*);
extern int XNextEvent(Display*, void*);
extern int XCloseDisplay(Display*);
int main(void){
    Display *d = XOpenDisplay(":0");
    if(!d){ printf("GXKEY: XOpenDisplay NULL\n"); fflush(stdout); _exit(7); }
    Window w = XCreateSimpleWindow(d, XDefaultRootWindow(d), 0,0, 260,120, 0,0, 0x20140a);
    XSelectInput(d, w, 0x1);           /* KeyPressMask */
    XMapWindow(d, w);
    GC gc = XCreateGC(d, w, 0, 0);
    XSetForeground(d, gc, 0xcc8844); XFillRectangle(d, w, gc, 8,8, 244,104); XFlush(d);
    printf("GXKEY: window mapped, waiting for real key input...\n"); fflush(stdout);
    unsigned codes[8]; int keys=0, tries=0;
    char ev[256];
    while(keys<3 && tries<200){
        if(XPending(d) > 0){
            XNextEvent(d, ev);
            if(*(int*)ev == 2){ codes[keys] = *(unsigned*)(ev+84); keys++; }
        } else {
            for(volatile int i=0;i<200000;i++) ;   /* brief wait for the pump */
            tries++;
        }
    }
    printf("GXKEY: got %d KeyPress event(s)", keys);
    for(int i=0;i<keys;i++) printf(" keycode=%u", codes[i]);
    printf("\n"); fflush(stdout);
    XCloseDisplay(d);
    _exit(keys==3 ? 55 : (keys>0?33:0));
}
