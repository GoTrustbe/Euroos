#include <pango/pangocairo.h>
#include <cairo.h>
#include <fontconfig/fontconfig.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <string.h>
#include <stdio.h>

/* Real Pango text layout (HarfBuzz shaping) rendered via cairo to an X11 window. */
#define W 520
#define H 200

int main(void){
  /* Make sure fontconfig knows about our one font without needing a dir scan. */
  if(!FcInit()){ printf("GPANGO: FcInit failed\n"); }
  if(!FcConfigAppFontAddFile(FcConfigGetCurrent(),
        (const FcChar8*)"/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"))
    printf("GPANGO: FcConfigAppFontAddFile failed\n");
  else
    printf("GPANGO: fontconfig registered DejaVuSans.ttf\n");

  Display *dpy = XOpenDisplay(NULL);
  if(!dpy){ printf("GPANGO: no display\n"); return 2; }
  int scr = DefaultScreen(dpy);
  Window win = XCreateSimpleWindow(dpy, RootWindow(dpy,scr), 80, 80, W, H, 0,
                                   BlackPixel(dpy,scr), 0x00141c2a);
  XStoreName(dpy, win, "EuroOS Pango");
  XSelectInput(dpy, win, ExposureMask);
  XMapWindow(dpy, win);
  GC gc = XCreateGC(dpy, win, 0, NULL);

  cairo_surface_t *surf = cairo_image_surface_create(CAIRO_FORMAT_ARGB32, W, H);
  cairo_t *cr = cairo_create(surf);
  cairo_set_source_rgb(cr, 0.078, 0.110, 0.165);
  cairo_paint(cr);

  PangoLayout *layout = pango_cairo_create_layout(cr);
  PangoFontDescription *desc = pango_font_description_from_string("DejaVu Sans Bold 30");
  pango_layout_set_font_description(layout, desc);
  pango_font_description_free(desc);
  pango_layout_set_markup(layout,
     "<span foreground='#5cc8ff'>EuroOS</span> "
     "<span foreground='#e8e8f0'>Pango</span>", -1);
  cairo_move_to(cr, 24, 24);
  pango_cairo_show_layout(cr, layout);

  /* second line, smaller, tests real shaping incl. accented + non-latin */
  PangoLayout *l2 = pango_cairo_create_layout(cr);
  PangoFontDescription *d2 = pango_font_description_from_string("DejaVu Sans 18");
  pango_layout_set_font_description(l2, d2);
  pango_font_description_free(d2);
  pango_layout_set_text(l2, "HarfBuzz shaping: fi fl AV To  \xC3\xA9\xC3\xA8\xC3\xAB  \xCE\xB1\xCE\xB2\xCE\xB3", -1);
  cairo_set_source_rgb(cr, 0.82, 0.82, 0.88);
  cairo_move_to(cr, 24, 96);
  pango_cairo_show_layout(cr, l2);
  cairo_surface_flush(surf);

  int cw, ch; pango_layout_get_pixel_size(layout, &cw, &ch);
  printf("GPANGO: layout1 pixel size %dx%d\n", cw, ch);

  unsigned char *data = cairo_image_surface_get_data(surf);
  XImage *img = XCreateImage(dpy, DefaultVisual(dpy,scr), 24, ZPixmap, 0,
                             (char*)data, W, H, 32, 0);
  XPutImage(dpy, win, gc, img, 0,0, 0,0, W, H);
  XFlush(dpy);
  printf("GPANGO: pango+harfbuzz text rendered (%dx%d)\n", W, H);

  XEvent ev;
  for(int i=0;i<3;i++){ if(XPending(dpy)){ XNextEvent(dpy,&ev); if(ev.type==Expose) XPutImage(dpy,win,gc,img,0,0,0,0,W,H); } }
  XFlush(dpy);
  return 0;
}
