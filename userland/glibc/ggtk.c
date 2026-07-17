#include <gtk/gtk.h>
#include <stdio.h>

/* A LIVE GTK3 app on EuroOS: a GtkDrawingArea that redraws a running counter every
   ~500 ms, proving the app runs persistently ALONGSIDE the desktop (not a static
   snapshot). Runs gtk_main forever; the desktop composites its window each frame. */

static int counter = 0;

static gboolean on_draw(GtkWidget *w, cairo_t *cr, gpointer data){
  (void)data;
  int width = gtk_widget_get_allocated_width(w);
  int height = gtk_widget_get_allocated_height(w);

  cairo_set_source_rgb(cr, 0.96, 0.96, 0.97);
  cairo_paint(cr);

  cairo_select_font_face(cr, "sans-serif", CAIRO_FONT_SLANT_NORMAL, CAIRO_FONT_WEIGHT_BOLD);
  cairo_set_source_rgb(cr, 0.21, 0.52, 0.89);
  cairo_set_font_size(cr, 22);
  cairo_move_to(cr, 24, 40);
  cairo_show_text(cr, "GTK 3 live on EuroOS");

  /* Big running counter — visibly increments while the desktop is up. */
  char buf[64];
  snprintf(buf, sizeof buf, "tick %d", counter);
  cairo_set_source_rgb(cr, 0.17, 0.20, 0.24);
  cairo_set_font_size(cr, 48);
  cairo_move_to(cr, 24, height / 2 + 24);
  cairo_show_text(cr, buf);

  /* A progress bar that sweeps with the counter. */
  cairo_set_source_rgb(cr, 0.21, 0.52, 0.89);
  int bw = (width - 48) * (counter % 20) / 19;
  cairo_rectangle(cr, 24, height - 40, bw, 14);
  cairo_fill(cr);
  return TRUE;
}

static GtkWidget *g_area;
static gboolean tick(gpointer data){
  (void)data;
  counter++;
  if (counter % 4 == 0) { printf("GGTK: live tick %d\n", counter); fflush(stdout); }
  gtk_widget_queue_draw(g_area);   /* schedule a redraw -> present -> desktop recomposites */
  return G_SOURCE_CONTINUE;        /* keep the timer running forever */
}

int main(int argc, char **argv){
  if(!gtk_init_check(&argc,&argv)){ printf("GGTK: gtk_init_check FAILED\n"); return 2; }
  printf("GGTK: gtk_init ok\n"); fflush(stdout);

  GtkWidget *win = gtk_window_new(GTK_WINDOW_TOPLEVEL);
  gtk_window_set_title(GTK_WINDOW(win), "EuroOS GTK");
  gtk_window_set_default_size(GTK_WINDOW(win), 480, 240);

  g_area = gtk_drawing_area_new();
  gtk_widget_set_size_request(g_area, 480, 240);
  g_signal_connect(g_area, "draw", G_CALLBACK(on_draw), NULL);
  gtk_container_add(GTK_CONTAINER(win), g_area);

  gtk_widget_show_all(win);
  printf("GGTK: window shown (live counter)\n"); fflush(stdout);

  g_timeout_add(500, tick, NULL);
  gtk_main();
  printf("GGTK: clean exit\n");
  return 0;
}
