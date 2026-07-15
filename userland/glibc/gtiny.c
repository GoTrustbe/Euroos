#include <unistd.h>
int main(void){ write(1,"GLIBC-DYNAMIC-OK\n",17); _exit(42); }
