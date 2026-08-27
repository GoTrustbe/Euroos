/* Minimal GLib declarations (no dev headers on this box; GLib ABI is stable). */
#include <stdio.h>
#include <string.h>
#include <unistd.h>
typedef void* gpointer; typedef const void* gconstpointer;
typedef unsigned int guint; typedef int gboolean;
typedef struct _GHashTable GHashTable;
typedef guint (*GHashFunc)(gconstpointer);
typedef gboolean (*GEqualFunc)(gconstpointer, gconstpointer);
extern GHashTable* g_hash_table_new(GHashFunc, GEqualFunc);
extern gboolean g_hash_table_insert(GHashTable*, gpointer, gpointer);
extern gpointer g_hash_table_lookup(GHashTable*, gconstpointer);
extern guint g_hash_table_size(GHashTable*);
extern void g_hash_table_destroy(GHashTable*);
extern guint g_str_hash(gconstpointer);
extern gboolean g_str_equal(gconstpointer, gconstpointer);

int main(void){
    GHashTable *h = g_hash_table_new(g_str_hash, g_str_equal);
    g_hash_table_insert(h, "os",   "EuroOS");
    g_hash_table_insert(h, "lang", "Rust");
    g_hash_table_insert(h, "kind", "sovereign");
    const char *v1 = g_hash_table_lookup(h, "os");
    const char *v2 = g_hash_table_lookup(h, "lang");
    guint n = g_hash_table_size(h);
    printf("GGLIB: GHashTable size=%u os=%s lang=%s\n", n, v1?v1:"(null)", v2?v2:"(null)");
    int ok = (n==3) && v1 && strcmp(v1,"EuroOS")==0 && v2 && strcmp(v2,"Rust")==0;
    g_hash_table_destroy(h);
    printf("GGLIB: GLib desktop-stack library -> %s\n", ok?"PASS":"FAIL");
    fflush(stdout);
    _exit(ok?55:1);
}
