#include <gtk/gtk.h>
#include <stdio.h>

/* A real GTK3 application on EuroOS: gtk_init, a window with a label + a button
   (Adwaita theme), and a GMainLoop (poll over the eventfd wakeup + the X fd).
   Kept on screen (no self-quit; the kernel run-deadline ends it) so the X server
   renders the widgets and we can screenshot the window. */

static gboolean quit_soon(gpointer data){
  (void)data;
  printf("GGTK: main loop ran (window rendered), quitting\n");
  fflush(stdout);
  gtk_main_quit();
  return G_SOURCE_REMOVE;
}

int main(int argc, char **argv){
  if(!gtk_init_check(&argc,&argv)){ printf("GGTK: gtk_init_check FAILED\n"); return 2; }
  printf("GGTK: gtk_init ok\n"); fflush(stdout);

  GtkWidget *win = gtk_window_new(GTK_WINDOW_TOPLEVEL);
  gtk_window_set_title(GTK_WINDOW(win), "EuroOS GTK");
  gtk_window_set_default_size(GTK_WINDOW(win), 460, 220);
  gtk_container_set_border_width(GTK_CONTAINER(win), 24);

  GtkWidget *box = gtk_box_new(GTK_ORIENTATION_VERTICAL, 16);
  gtk_container_add(GTK_CONTAINER(win), box);

  GtkWidget *title = gtk_label_new(NULL);
  gtk_label_set_markup(GTK_LABEL(title),
    "<span size='24000' weight='bold' foreground='#3584e4'>GTK 3</span>"
    "<span size='24000' weight='bold'> on EuroOS</span>");
  gtk_box_pack_start(GTK_BOX(box), title, FALSE, FALSE, 0);

  GtkWidget *sub = gtk_label_new("A real GTK toolkit window: labels, a button, the Adwaita theme.");
  gtk_label_set_line_wrap(GTK_LABEL(sub), TRUE);
  gtk_box_pack_start(GTK_BOX(box), sub, FALSE, FALSE, 0);

  GtkWidget *btn = gtk_button_new_with_label("Sovereign by design");
  gtk_box_pack_start(GTK_BOX(box), btn, FALSE, FALSE, 0);

  gtk_widget_show_all(win);
  printf("GGTK: window shown (460x220, label+button)\n"); fflush(stdout);

  g_timeout_add(1500, quit_soon, NULL); /* let the window render, then quit */
  gtk_main();
  printf("GGTK: clean exit\n");
  return 0;
}
