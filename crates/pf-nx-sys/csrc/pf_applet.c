// The applet service's operation mode, asked of the system. libnx's appletGetOperationMode
// returns a copy it refreshes only on OperationModeChanged; a dock change during sleep sends none.
#include <switch.h>

// ICommonStateGetter 5, GetOperationMode (libnx 4.12.0 applet.c, `_appletGetOperationMode`).
// 0 with `out` holding AppletOperationMode; libnx's Result otherwise.
uint32_t pf_applet_operation_mode(uint8_t *out) {
    u8 mode = 0;
    Result rc = serviceDispatchOut(appletGetServiceSession_CommonStateGetter(), 5, mode);
    if (R_SUCCEEDED(rc)) {
        *out = mode;
    }
    return rc;
}
