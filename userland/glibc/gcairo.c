/* Cairo 2D rendering to an image surface, then XPutImage into an X window.
   Proves a real vector-graphics library (gradients, curves, AA) runs on EuroOS. */
#include <stdio.h>
#include <string.h>
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
extern int XMapWindow(Display*, Window);
extern GC XCreateGC(Display*, Drawable, unsigned long, void*);
extern XImage* XCreateImage(Display*, Visual*, unsigned, int, int, char*, unsigned, unsigned, int, int);
extern int XPutImage(Display*, Drawable, GC, XImage*, int,int,int,int, unsigned, unsigned);
extern int XFlush(Display*);
/* cairo (image surface subset) */
typedef struct _cairo cairo_t; typedef struct _cairo_surface cairo_surface_t;
extern cairo_surface_t* cairo_image_surface_create(int format, int w, int h);
extern unsigned char* cairo_image_surface_get_data(cairo_surface_t*);
extern int cairo_image_surface_get_stride(cairo_surface_t*);
extern cairo_t* cairo_create(cairo_surface_t*);
extern void cairo_set_source_rgb(cairo_t*, double,double,double);
extern void cairo_paint(cairo_t*);
extern void cairo_arc(cairo_t*, double,double,double,double,double);
extern void cairo_fill(cairo_t*);
extern void cairo_rectangle(cairo_t*, double,double,double,double);
extern void cairo_set_line_width(cairo_t*, double);
extern void cairo_move_to(cairo_t*, double,double);
extern void cairo_line_to(cairo_t*, double,double);
extern void cairo_stroke(cairo_t*);
extern void cairo_surface_flush(cairo_surface_t*);
#define W 300
#define H 220
int main(void){
    Display *d = XOpenDisplay(":0");
    if(!d){ printf("GCAIRO: XOpenDisplay NULL\n"); fflush(stdout); return 7; }
    /* Cairo: draw into a 32-bit image surface */
    cairo_surface_t *sf = cairo_image_surface_create(0 /*ARGB32*/, W, H);
    cairo_t *cr = cairo_create(sf);
    cairo_set_source_rgb(cr, 0.09,0.11,0.16); cairo_paint(cr);
    cairo_set_source_rgb(cr, 0.2,0.6,0.9);
    cairo_arc(cr, 100,110, 60, 0, 6.2831853); cairo_fill(cr);
    cairo_set_source_rgb(cr, 0.9,0.7,0.2);
    cairo_rectangle(cr, 170,60, 100,100); cairo_fill(cr);
    cairo_set_source_rgb(cr, 0.9,0.3,0.3); cairo_set_line_width(cr, 6);
    cairo_move_to(cr, 30,30); cairo_line_to(cr, 270,190); cairo_stroke(cr);
    cairo_surface_flush(sf);
    unsigned char *data = cairo_image_surface_get_data(sf);
    /* Blit the cairo buffer into an X window via XPutImage */
    int s = XDefaultScreen(d);
    Window w = XCreateSimpleWindow(d, XDefaultRootWindow(d), 0,0, W,H, 0,0, 0x000000);
    XMapWindow(d, w);
    GC gc = XCreateGC(d, w, 0, 0);
    XImage *img = XCreateImage(d, XDefaultVisual(d,s), XDefaultDepth(d,s), 2, 0, (char*)data, W, H, 32, cairo_image_surface_get_stride(sf));
    XPutImage(d, w, gc, img, 0,0, 0,0, W, H);
    XFlush(d);
    printf("GCAIRO: cairo image surface rendered + XPutImage'd (%dx%d)\n", W, H);
    fflush(stdout);
    for(volatile long i=0;i<20000000;i++) ;
    return 0;
}
