#pragma once
#include <time.h>
#include <errno.h>
#ifndef TIMER_ABSTIME
#define TIMER_ABSTIME 1
#endif
/* newlib on Horizon has no clock_nanosleep: an absolute sleep becomes a relative one. */
static inline int pf_nx_clock_nanosleep(clockid_t c, int flags, const struct timespec *t, struct timespec *rem)
{
	struct timespec d = *t;
	if (flags & TIMER_ABSTIME) {
		struct timespec now;
		clock_gettime(c, &now);
		d.tv_sec = t->tv_sec - now.tv_sec;
		d.tv_nsec = t->tv_nsec - now.tv_nsec;
		if (d.tv_nsec < 0) { d.tv_nsec += 1000000000L; d.tv_sec -= 1; }
		if (d.tv_sec < 0) return 0;
	}
	return nanosleep(&d, rem) == 0 ? 0 : errno;
}
#define clock_nanosleep pf_nx_clock_nanosleep
