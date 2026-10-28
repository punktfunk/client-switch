// What Skia calls that newlib and libnx lack, appended to libskia.a by scripts/build-skia.sh.
// Weak, so a title that defines one keeps its own.
#include <stdlib.h>
#include <sys/mman.h>
#include <unistd.h>

// Moves the descriptor's offset and puts it back: not safe beside another reader of the same fd.
__attribute__((weak)) ssize_t pread(int fd, void *buf, size_t n, off_t off) {
    off_t at = lseek(fd, 0, SEEK_CUR);
    if (at < 0 || lseek(fd, off, SEEK_SET) < 0) return -1;
    ssize_t got = read(fd, buf, n);
    lseek(fd, at, SEEK_SET);
    return got;
}

// Skia and ICU map files read-only, so a heap copy serves; munmap frees it.
__attribute__((weak)) void *mmap(void *addr, size_t len, int prot, int flags, int fd, off_t off) {
    (void)addr, (void)prot, (void)flags;
    void *p = malloc(len);
    if (p && pread(fd, p, len, off) == (ssize_t)len) return p;
    free(p);
    return MAP_FAILED;
}

__attribute__((weak)) int munmap(void *addr, size_t len) {
    (void)len;
    free(addr);
    return 0;
}

__attribute__((weak)) int posix_madvise(void *addr, size_t len, int advice) {
    (void)addr, (void)len, (void)advice;
    return 0;
}
