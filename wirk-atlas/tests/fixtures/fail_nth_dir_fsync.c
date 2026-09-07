#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <unistd.h>

int fsync(int fd) {
    static int (*real_fsync)(int) = NULL;
    static int directory_calls = 0;
    if (!real_fsync) real_fsync = dlsym(RTLD_NEXT, "fsync");
    struct stat st;
    if (fstat(fd, &st) == 0 && S_ISDIR(st.st_mode)) {
        directory_calls++;
        const char *configured = getenv("W1_FAIL_DIRECTORY_FSYNC_CALL");
        if (configured && directory_calls == atoi(configured)) {
            errno = EIO;
            return -1;
        }
    }
    return real_fsync(fd);
}
