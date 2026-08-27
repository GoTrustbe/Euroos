#include <stdio.h>
#include <fcntl.h>
#include <unistd.h>
#include <string.h>
int main(void){
    const char *path="/tmp/euro-glibc.txt";
    const char *msg="EuroOS file I/O works via real glibc\n";
    int fd=open(path,O_CREAT|O_WRONLY|O_TRUNC,0644);
    if(fd<0){ printf("GFILE: open(write) FAILED\n"); return 1; }
    ssize_t w=write(fd,msg,strlen(msg));
    close(fd);
    char buf[128]={0};
    fd=open(path,O_RDONLY);
    if(fd<0){ printf("GFILE: open(read) FAILED\n"); return 1; }
    ssize_t r=read(fd,buf,sizeof(buf)-1);
    close(fd);
    int ok = (w==(ssize_t)strlen(msg)) && (r==w) && (strcmp(buf,msg)==0);
    printf("GFILE: wrote %zd read %zd bytes, roundtrip %s\n", w, r, ok?"PASS":"FAIL");
    fflush(stdout);
    _exit(ok?44:1);
}
