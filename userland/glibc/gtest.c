#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
static int cmp(const void *a, const void *b){ return *(const int*)a - *(const int*)b; }
int main(void){
    printf("GTEST: printf + stdio ok\n");
    char *big = malloc(1<<20); memset(big,0x41,1<<20);
    printf("GTEST: malloc 1MiB ok first=%c last=%c\n", big[0], big[(1<<20)-1]);
    int v[8]={5,3,8,1,9,2,7,4}; qsort(v,8,sizeof(int),cmp);
    printf("GTEST: qsort -> %d %d %d ... %d\n", v[0],v[1],v[2],v[7]);
    char buf[64]; snprintf(buf,sizeof buf,"%d-%x-%s", 42, 255, "euro");
    long n = strtol("12345", NULL, 10);
    printf("GTEST: snprintf='%s' strtol=%ld\n", buf, n);
    fflush(stdout);
    _exit(55);
}
