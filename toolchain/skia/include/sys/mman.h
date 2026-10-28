/* The mmap surface Skia and ICU name, which newlib lacks. compat.c defines it. */
#pragma once
#include <sys/types.h>

#define PROT_READ 1
#define MAP_SHARED 1
#define MAP_PRIVATE 2
#define MAP_FAILED ((void *)-1)
#define POSIX_MADV_RANDOM 1

#ifdef __cplusplus
extern "C" {
#endif
void *mmap(void *addr, size_t len, int prot, int flags, int fd, off_t off);
int munmap(void *addr, size_t len);
int posix_madvise(void *addr, size_t len, int advice);
#ifdef __cplusplus
}
#endif
