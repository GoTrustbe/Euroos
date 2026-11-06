/* hello-cli: the smallest EuroOS app. Built with the ordinary host gcc against
   glibc; EuroOS serves the glibc runtime itself. Prints two lines and exits 7,
   which the kernel's [app] self-test checks. */
#include <stdio.h>
#include <unistd.h>

int main(void) {
    printf("HELLO from an installed EuroApp (pid %d)\n", (int)getpid());
    printf("built by a third-party developer, signed with their own key, installed by eupkg\n");
    fflush(stdout);
    return 7;
}
