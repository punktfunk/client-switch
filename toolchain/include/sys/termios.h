/* devkitA64's <termios.h> includes this, which newlib leaves out. aws-lc's console.c (password
 * prompts) compiles against it; Horizon has no terminal, and nothing links tcgetattr/tcsetattr. */
#pragma once

typedef unsigned int tcflag_t;
typedef unsigned char cc_t;

struct termios {
    tcflag_t c_iflag, c_oflag, c_cflag, c_lflag;
    cc_t c_cc[20];
};

#define ECHO 0x8
#define TCSANOW 0

int tcgetattr(int fd, struct termios *t);
int tcsetattr(int fd, int action, const struct termios *t);
