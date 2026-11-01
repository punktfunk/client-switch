// The system keyboard applet as one call the Rust side binds (src/swkbd.rs). libnx's
// SwkbdConfig is a versioned argument block it sizes per firmware, so it stays on this side.
#include <switch.h>

// Shows the keyboard over the title and blocks until the player closes it. 0 with `out`
// holding the committed UTF-8 text, NUL-terminated; libnx's Result otherwise, cancel included.
uint32_t pf_swkbd_ask(const char *guide, const char *ok, uint32_t max_chars, char *out, size_t out_len) {
    SwkbdConfig kbd;
    out[0] = '\0';
    Result rc = swkbdCreate(&kbd, 0);
    if (R_FAILED(rc)) {
        return rc;
    }
    swkbdConfigMakePresetDefault(&kbd);
    swkbdConfigSetGuideText(&kbd, guide);
    swkbdConfigSetOkButtonText(&kbd, ok);
    swkbdConfigSetStringLenMax(&kbd, max_chars);
    rc = swkbdShow(&kbd, out, out_len);
    swkbdClose(&kbd);
    return rc;
}
