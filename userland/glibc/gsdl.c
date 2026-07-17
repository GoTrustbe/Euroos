#include <SDL2/SDL.h>
#include <stdio.h>

/* A real SDL2 app on EuroOS: create an X11 window, draw a gradient + moving box
   to its surface (software framebuffer -> XPutImage), proving the toolkit
   foundation is not GTK-specific. */
int main(int argc, char **argv){
  (void)argc;(void)argv;
  if (SDL_Init(SDL_INIT_VIDEO) != 0){ printf("GSDL: SDL_Init FAILED: %s\n", SDL_GetError()); return 2; }
  printf("GSDL: SDL_Init ok\n"); fflush(stdout);
  SDL_Window *win = SDL_CreateWindow("EuroOS SDL", 0, 0, 440, 260, SDL_WINDOW_SHOWN);
  if (!win){ printf("GSDL: CreateWindow FAILED: %s\n", SDL_GetError()); return 3; }
  printf("GSDL: window created\n"); fflush(stdout);
  SDL_Surface *s = SDL_GetWindowSurface(win);
  if (!s){ printf("GSDL: GetWindowSurface FAILED: %s\n", SDL_GetError()); return 4; }
  int frame = 0;
  for(;;){
    SDL_Event e; while (SDL_PollEvent(&e)) { if (e.type == SDL_QUIT) goto done; }
    Uint32 *px = (Uint32*)s->pixels;
    int pitch = s->pitch/4;
    for (int y=0;y<s->h;y++) for (int x=0;x<s->w;x++)
      px[y*pitch+x] = SDL_MapRGB(s->format, (x+frame)&0xff, (y+frame)&0xff, 0x80);
    /* a moving white box */
    int bx = (frame*2) % (s->w-40), by = s->h/2-20;
    for (int y=by;y<by+40;y++) for (int x=bx;x<bx+40;x++) px[y*pitch+x] = SDL_MapRGB(s->format,255,255,255);
    SDL_UpdateWindowSurface(win);
    if (frame % 20 == 0){ printf("GSDL: frame %d rendered\n", frame); fflush(stdout); }
    frame++;
    SDL_Delay(50);
  }
done:
  SDL_DestroyWindow(win); SDL_Quit();
  return 0;
}
