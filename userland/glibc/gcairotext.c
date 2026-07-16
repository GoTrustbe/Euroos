/* Cairo + FreeType TEXT rendering -> XPutImage. Real font rasterization on EuroOS. */
#include <stdio.h>
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
/* cairo */
typedef struct _cairo cairo_t; typedef struct _cairo_surface cairo_surface_t;
typedef struct _cairo_font_face cairo_font_face_t;
extern cairo_surface_t* cairo_image_surface_create(int,int,int);
extern unsigned char* cairo_image_surface_get_data(cairo_surface_t*);
extern int cairo_image_surface_get_stride(cairo_surface_t*);
extern cairo_t* cairo_create(cairo_surface_t*);
extern void cairo_set_source_rgb(cairo_t*, double,double,double);
extern void cairo_paint(cairo_t*);
extern void cairo_set_font_face(cairo_t*, cairo_font_face_t*);
extern void cairo_set_font_size(cairo_t*, double);
extern void cairo_move_to(cairo_t*, double,double);
extern void cairo_show_text(cairo_t*, const char*);
extern void cairo_surface_flush(cairo_surface_t*);
extern cairo_font_face_t* cairo_ft_font_face_create_for_ft_face(void*, int);
/* freetype */
extern int FT_Init_FreeType(void**);
extern int FT_New_Face(void*, const char*, long, void**);
#define W 460
#define H 160
int main(void){
    Display *d = XOpenDisplay(":0");
    if(!d){ printf("GCTEXT: XOpenDisplay NULL\n"); fflush(stdout); return 7; }
    void *ftlib=0, *ftface=0;
    if(FT_Init_FreeType(&ftlib)){ printf("GCTEXT: FT_Init failed\n"); fflush(stdout); return 1; }
    if(FT_New_Face(ftlib, "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 0, &ftface)){
        printf("GCTEXT: FT_New_Face failed\n"); fflush(stdout); return 2; }
    cairo_surface_t *sf = cairo_image_surface_create(0, W, H);
    cairo_t *cr = cairo_create(sf);
    cairo_set_source_rgb(cr, 0.10,0.12,0.18); cairo_paint(cr);
    cairo_font_face_t *ff = cairo_ft_font_face_create_for_ft_face(ftface, 0);
    cairo_set_font_face(cr, ff);
    cairo_set_source_rgb(cr, 0.4,0.8,1.0);
    cairo_set_font_size(cr, 44);
    cairo_move_to(cr, 24, 70); cairo_show_text(cr, "EuroOS");
    cairo_set_source_rgb(cr, 0.85,0.85,0.9);
    cairo_set_font_size(cr, 24);
    cairo_move_to(cr, 26, 115); cairo_show_text(cr, "X11 + Cairo + FreeType");
    cairo_surface_flush(sf);
    int s = XDefaultScreen(d);
    Window w = XCreateSimpleWindow(d, XDefaultRootWindow(d), 0,0, W,H, 0,0, 0);
    XMapWindow(d, w);
    GC gc = XCreateGC(d, w, 0, 0);
    XImage *img = XCreateImage(d, XDefaultVisual(d,s), XDefaultDepth(d,s), 2, 0,
                               (char*)cairo_image_surface_get_data(sf), W, H, 32, cairo_image_surface_get_stride(sf));
    XPutImage(d, w, gc, img, 0,0, 0,0, W, H);
    XFlush(d);
    printf("GCTEXT: cairo+freetype text rendered (%dx%d)\n", W, H);
    fflush(stdout);
    for(volatile long i=0;i<20000000;i++) ;
    return 0;
}
