#pragma once
/* Horizon has no dynamic loader: every lookup fails, and Granite takes the title's own
   vkGetInstanceProcAddr instead. */
#define RTLD_LAZY 1
#define RTLD_NOW 2
#define RTLD_LOCAL 0
#define RTLD_GLOBAL 0x100
#define RTLD_NOLOAD 4
static inline void *dlopen(const char *f, int m) { (void)f; (void)m; return 0; }
static inline void *dlsym(void *h, const char *s) { (void)h; (void)s; return 0; }
static inline int dlclose(void *h) { (void)h; return 0; }
static inline char *dlerror(void) { return (char *)"no dynamic loader on Horizon"; }
