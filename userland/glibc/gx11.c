#include <stdio.h>
#include <unistd.h>
typedef struct _XDisplay Display;
extern Display* XOpenDisplay(const char*);
extern int XCloseDisplay(Display*);
extern int XConnectionNumber(Display*);
int main(void){
    Display *d = XOpenDisplay(":0");   /* connect to the X server on DISPLAY :0 */
    if(!d){ printf("GX11: XOpenDisplay(:0) returned NULL (no X server yet)\n"); fflush(stdout); _exit(7); }
    printf("GX11: connected to X server, fd=%d\n", XConnectionNumber(d));
    XCloseDisplay(d);
    fflush(stdout);
    _exit(0);
}
