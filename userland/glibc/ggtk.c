#include <gtk/gtk.h>
#include <stdio.h>

/* A real GTK3 application on EuroOS: gtk_init + a window whose content is a
   GtkDrawingArea with a custom cairo draw callback. The callback does an
   explicit solid fill (which core X can do), a diagonal line, and text — so we
   can see exactly which cairo operations reach the framebuffer through the
   in-kernel X server (isolates "child draws at all?" vs "only glyphs fail"). */

static gboolean on_draw(GtkWidget *w, cairo_t *cr, gpointer data){
  (void)data;
  int width = gtk_widget_get_allocated_width(w);
  int height = gtk_widget_get_allocated_height(w);
  printf("GGTK: draw callback fired, area %dx%d\n", width, height); fflush(stdout);

  /* Solid fill — cairo maps this to core-X PolyFillRectangle. */
  cairo_set_source_rgb(cr, 0.21, 0.52, 0.89); /* GNOME blue */
  cairo_paint(cr);

  /* A filled inner rectangle in a second colour. */
  cairo_set_source_rgb(cr, 0.95, 0.76, 0.15); /* amber */
  cairo_rectangle(cr, 20, 20, width - 40, 40);
  cairo_fill(cr);

  /* A diagonal stroked line. */
  cairo_set_source_rgb(cr, 1, 1, 1);
  cairo_set_line_width(cr, 4);
  cairo_move_to(cr, 20, height - 20);
  cairo_line_to(cr, width - 20, 80);
  cairo_stroke(cr);

  /* Text (glyphs) — needs XRender or cairo's image fallback. */
  cairo_set_source_rgb(cr, 1, 1, 1);
  cairo_select_font_face(cr, "sans-serif", CAIRO_FONT_SLANT_NORMAL, CAIRO_FONT_WEIGHT_BOLD);
  cairo_set_font_size(cr, 28);
  cairo_move_to(cr, 30, 130);
  cairo_show_text(cr, "GTK on EuroOS");
  return TRUE;
}

static gboolean quit_soon(gpointer data){
  (void)data;
  printf("GGTK: main loop ran (window rendered), quitting\n"); fflush(stdout);
  gtk_main_quit();
  return G_SOURCE_REMOVE;
}

int main(int argc, char **argv){
  if(!gtk_init_check(&argc,&argv)){ printf("GGTK: gtk_init_check FAILED\n"); return 2; }
  printf("GGTK: gtk_init ok\n"); fflush(stdout);

  GtkWidget *win = gtk_window_new(GTK_WINDOW_TOPLEVEL);
  gtk_window_set_title(GTK_WINDOW(win), "EuroOS GTK");
  gtk_window_set_default_size(GTK_WINDOW(win), 460, 220);

  GtkWidget *area = gtk_drawing_area_new();
  gtk_widget_set_size_request(area, 460, 220);
  g_signal_connect(area, "draw", G_CALLBACK(on_draw), NULL);
  gtk_container_add(GTK_CONTAINER(win), area);

  gtk_widget_show_all(win);
  printf("GGTK: window shown (460x220, drawing area)\n"); fflush(stdout);

  g_timeout_add(1500, quit_soon, NULL);
  gtk_main();
  printf("GGTK: clean exit\n");
  return 0;
}
