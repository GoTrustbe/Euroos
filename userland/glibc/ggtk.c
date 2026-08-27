#include <gtk/gtk.h>
#include <stdio.h>

/* A LIVE, INTERACTIVE GTK3 app on EuroOS: a drawing area shows a running counter
   (redrawn every ~500 ms, proving it runs alongside the desktop), plus a real
   GtkButton that resets the counter when clicked — the click is routed from the
   EuroOS desktop into the X server and dispatched to the button by GTK. */

static int counter = 0;
static GtkWidget *g_area;

static gboolean on_draw(GtkWidget *w, cairo_t *cr, gpointer data){
  (void)data;
  int width = gtk_widget_get_allocated_width(w);
  int height = gtk_widget_get_allocated_height(w);
  cairo_set_source_rgb(cr, 0.96, 0.96, 0.97);
  cairo_paint(cr);

  cairo_select_font_face(cr, "sans-serif", CAIRO_FONT_SLANT_NORMAL, CAIRO_FONT_WEIGHT_BOLD);
  cairo_set_source_rgb(cr, 0.21, 0.52, 0.89);
  cairo_set_font_size(cr, 20);
  cairo_move_to(cr, 20, 34);
  cairo_show_text(cr, "GTK 3 live on EuroOS");

  char buf[64];
  snprintf(buf, sizeof buf, "tick %d", counter);
  cairo_set_source_rgb(cr, 0.17, 0.20, 0.24);
  cairo_set_font_size(cr, 44);
  cairo_move_to(cr, 20, height - 24);
  cairo_show_text(cr, buf);
  return TRUE;
}

static gboolean tick(gpointer data){
  (void)data;
  counter++;
  if (counter % 4 == 0) { printf("GGTK: live tick %d\n", counter); fflush(stdout); }
  gtk_widget_queue_draw(g_area);
  return G_SOURCE_CONTINUE;
}

static void on_reset(GtkButton *b, gpointer data){
  (void)b; (void)data;
  counter = 0;
  printf("GGTK: BUTTON CLICKED -> counter reset\n"); fflush(stdout);
  gtk_widget_queue_draw(g_area);
}

static gboolean on_key(GtkWidget *w, GdkEventKey *ev, gpointer data){
  (void)w; (void)data;
  printf("GGTK: KEY keyval=%u '%s'\n", ev->keyval, ev->string ? ev->string : ""); fflush(stdout);
  return FALSE;
}
static void on_entry_changed(GtkEditable *e, gpointer data){
  (void)data;
  printf("GGTK: entry = '%s'\n", gtk_entry_get_text(GTK_ENTRY(e))); fflush(stdout);
}

int main(int argc, char **argv){
  if(!gtk_init_check(&argc,&argv)){ printf("GGTK: gtk_init_check FAILED\n"); return 2; }
  printf("GGTK: gtk_init ok\n"); fflush(stdout);

  GtkWidget *win = gtk_window_new(GTK_WINDOW_TOPLEVEL);
  gtk_window_set_title(GTK_WINDOW(win), "EuroOS GTK");
  gtk_window_set_default_size(GTK_WINDOW(win), 480, 290);
  g_signal_connect(win, "key-press-event", G_CALLBACK(on_key), NULL);

  GtkWidget *box = gtk_box_new(GTK_ORIENTATION_VERTICAL, 8);
  gtk_container_set_border_width(GTK_CONTAINER(box), 12);
  gtk_container_add(GTK_CONTAINER(win), box);

  g_area = gtk_drawing_area_new();
  gtk_widget_set_size_request(g_area, 456, 120);
  g_signal_connect(g_area, "draw", G_CALLBACK(on_draw), NULL);
  gtk_box_pack_start(GTK_BOX(box), g_area, TRUE, TRUE, 0);

  GtkWidget *entry = gtk_entry_new();
  gtk_entry_set_placeholder_text(GTK_ENTRY(entry), "type here...");
  g_signal_connect(entry, "changed", G_CALLBACK(on_entry_changed), NULL);
  gtk_box_pack_start(GTK_BOX(box), entry, FALSE, FALSE, 0);

  GtkWidget *btn = gtk_button_new_with_label("Reset counter");
  g_signal_connect(btn, "clicked", G_CALLBACK(on_reset), NULL);
  gtk_box_pack_start(GTK_BOX(box), btn, FALSE, FALSE, 0);

  gtk_widget_show_all(win);
  gtk_widget_grab_focus(entry);
  printf("GGTK: window shown (counter + entry + button)\n"); fflush(stdout);

  g_timeout_add(500, tick, NULL);
  gtk_main();
  printf("GGTK: clean exit\n");
  return 0;
}
