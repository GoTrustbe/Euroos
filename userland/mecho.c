/* EuroOS — musl `echo`: print the arguments (argv[1..]) separated by spaces.
 * Clean source for pipe demos (no noise header). */
#include <stdio.h>

int main(int argc, char **argv) {
    for (int i = 1; i < argc; i++) {
        fputs(argv[i], stdout);
        if (i < argc - 1) {
            fputc(' ', stdout);
        }
    }
    fputc('\n', stdout);
    return 0;
}
