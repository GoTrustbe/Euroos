#include <gtk/gtk.h>
#include <stdio.h>
static gboolean quit_soon(gpointer data){ printf("GGTK: main loop ran, quitting\n"); gtk_main_quit(); return G_SOURCE_REMOVE; }
int main(int argc, char **argv){
  if(!gtk_init_check(&argc,&argv)){ printf("GGTK: gtk_init_check FAILED\n"); return 2; }
  printf("GGTK: gtk_init ok\n");
  GtkWidget *win = gtk_window_new(GTK_WINDOW_TOPLEVEL);
  gtk_window_set_title(GTK_WINDOW(win), "EuroOS GTK");
  gtk_window_set_default_size(GTK_WINDOW(win), 420, 180);
  GtkWidget *lbl = gtk_label_new("GTK on EuroOS");
  gtk_container_add(GTK_CONTAINER(win), lbl);
  gtk_widget_show_all(win);
  printf("GGTK: window shown\n");
  g_timeout_add(1200, quit_soon, NULL);
  gtk_main();
  printf("GGTK: clean exit\n");
  return 0;
}
