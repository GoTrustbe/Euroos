#include <stdio.h>
#include <sys/socket.h>
#include <unistd.h>
#include <string.h>
int main(void){
    int sv[2];
    if(socketpair(AF_UNIX, SOCK_STREAM, 0, sv)!=0){ printf("GUNIX: socketpair FAILED\n"); fflush(stdout); return 1; }
    const char *msg = "AF_UNIX round-trip on EuroOS";
    ssize_t w = write(sv[0], msg, strlen(msg));
    char buf[64]={0};
    ssize_t r = read(sv[1], buf, sizeof(buf)-1);
    int ok = (w==(ssize_t)strlen(msg)) && (r==w) && strcmp(buf,msg)==0;
    printf("GUNIX: socketpair(AF_UNIX) wrote %zd read %zd bytes, roundtrip %s\n", w, r, ok?"PASS":"FAIL");
    fflush(stdout);
    _exit(ok?67:1);
}
