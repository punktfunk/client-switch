// The overclock module's IPC, for src/ocmod.rs. libnx's dispatch calls are inline, so they stay on
// this side. sys-clk and Horizon OC share commands 0, 1 and 8 under different service names.
#include <switch.h>

#define CMD_API_VERSION 0
#define CMD_VERSION_STRING 1
#define CMD_SET_OVERRIDE 8
// Atmosphère's sm extension.
#define SM_HAS_SERVICE 65100

// Whether `name` is registered, in `out`. smGetService on a missing name waits for it for good,
// so this comes first.
uint32_t pf_ocmod_has(const char *name, uint8_t *out) {
    Result rc = smInitialize();
    if (R_FAILED(rc)) {
        return rc;
    }
    SmServiceName encoded = smEncodeName(name);
    if (hosversionAtLeast(12, 0, 0)) {
        rc = tipcDispatchInOut(smGetServiceSessionTipc(), SM_HAS_SERVICE, encoded, *out);
    } else {
        rc = serviceDispatchInOut(smGetServiceSession(), SM_HAS_SERVICE, encoded, *out);
    }
    smExit();
    return rc;
}

static Result open_module(const char *name, Service *srv) {
    Result rc = smInitialize();
    if (R_FAILED(rc)) {
        return rc;
    }
    rc = smGetService(srv, name);
    smExit();
    return rc;
}

// The module's API version and its version string, NUL-terminated in `version`.
uint32_t pf_ocmod_info(const char *name, uint32_t *api, char *version, size_t version_len) {
    Service srv;
    Result rc = open_module(name, &srv);
    if (R_FAILED(rc)) {
        return rc;
    }
    rc = serviceDispatchOut(&srv, CMD_API_VERSION, *api);
    if (R_SUCCEEDED(rc)) {
        rc = serviceDispatch(&srv, CMD_VERSION_STRING,
            .buffer_attrs = { SfBufferAttr_HipcAutoSelect | SfBufferAttr_Out },
            .buffers = { { version, version_len } },
        );
    }
    serviceClose(&srv);
    return rc;
}

// Asks the module to hold `module` at `hz`; 0 hands the clock back.
uint32_t pf_ocmod_override(const char *name, uint32_t module, uint32_t hz) {
    Service srv;
    Result rc = open_module(name, &srv);
    if (R_FAILED(rc)) {
        return rc;
    }
    const struct {
        uint32_t module;
        uint32_t hz;
    } args = { module, hz };
    rc = serviceDispatchIn(&srv, CMD_SET_OVERRIDE, args);
    serviceClose(&srv);
    return rc;
}
