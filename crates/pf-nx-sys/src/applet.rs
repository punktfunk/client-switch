//! The applet service: operation mode, applet type, focus, the main-loop pump, status hooks and
//! the exit lock. Externs from `services/applet.h`, libnx 4.12.0.
//!
//! libnx keeps the hook list without a lock and calls it from inside `appletMainLoop`, so a
//! [`Hook`] is made, dropped and pumped on one thread: the main thread.

#[cfg(target_os = "horizon")]
use core::ffi::c_void;

/// `AppletOperationMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationMode {
    Handheld,
    /// `AppletOperationMode_Console`: in the dock, on the TV.
    Docked,
}

impl OperationMode {
    pub fn from_raw(v: u32) -> Self {
        if v == 1 {
            Self::Docked
        } else {
            Self::Handheld
        }
    }
}

/// `AppletType`. `LibraryApplet` is a title launched from the album: applet mode (D13).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppletType {
    Application,
    SystemApplet,
    LibraryApplet,
    OverlayApplet,
    SystemApplication,
    Default,
    Unknown,
}

impl AppletType {
    pub fn from_raw(v: i32) -> Self {
        match v {
            0 => Self::Application,
            1 => Self::SystemApplet,
            2 => Self::LibraryApplet,
            3 => Self::OverlayApplet,
            4 => Self::SystemApplication,
            -1 => Self::Default,
            _ => Self::Unknown,
        }
    }
}

/// `AppletFocusState`. `Background` is the HOME menu or sleep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusState {
    InFocus,
    /// A library applet is open over the title.
    OutOfFocus,
    Background,
}

impl FocusState {
    pub fn from_raw(v: u32) -> Self {
        match v {
            2 => Self::OutOfFocus,
            3 => Self::Background,
            _ => Self::InFocus,
        }
    }
}

/// The `AppletHookType`s a hook is told about. libnx 4.12.0 has no suspend hook: sleep arrives
/// as `FocusState` (`Background`), wake as `Resume`. Capture-button, screenshot and
/// request-to-display hooks are not forwarded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookEvent {
    Exit,
    OperationMode,
    PerformanceMode,
    FocusState,
    Resume,
}

impl HookEvent {
    /// `AppletHookType` → event; `None` for the types not forwarded.
    pub fn from_raw(v: u32) -> Option<Self> {
        Some(match v {
            0 => Self::FocusState,
            1 => Self::OperationMode,
            2 => Self::PerformanceMode,
            3 => Self::Exit,
            4 => Self::Resume,
            _ => return None,
        })
    }
}

#[cfg(target_os = "horizon")]
mod ffi {
    use core::ffi::c_void;

    use super::AppletHookCookie;

    pub type AppletHookFn = unsafe extern "C" fn(hook: u32, param: *mut c_void);

    extern "C" {
        pub fn appletGetAppletType() -> i32;
        pub fn appletMainLoop() -> bool;
        pub fn appletHook(cookie: *mut AppletHookCookie, callback: AppletHookFn, param: *mut c_void);
        pub fn appletUnhook(cookie: *mut AppletHookCookie);
        pub fn appletGetOperationMode() -> u32;
        pub fn appletGetFocusState() -> u32;
        pub fn appletLockExit() -> u32;
        pub fn appletUnlockExit() -> u32;
    }
}

/// `AppletHookCookie`: `{ next, callback, param }`, three pointers. Only libnx reads it.
#[repr(C)]
pub struct AppletHookCookie(pub [*mut core::ffi::c_void; 3]);

/// The operation mode as of the last [`main_loop`].
#[cfg(target_os = "horizon")]
pub fn operation_mode() -> OperationMode {
    // SAFETY: reads libnx's cached state; no arguments.
    OperationMode::from_raw(unsafe { ffi::appletGetOperationMode() })
}

#[cfg(target_os = "horizon")]
pub fn applet_type() -> AppletType {
    // SAFETY: reads the type libnx stored at startup; no arguments.
    AppletType::from_raw(unsafe { ffi::appletGetAppletType() })
}

/// Pumps one applet message and runs the hooks; `false` once exit is requested.
#[cfg(target_os = "horizon")]
pub fn main_loop() -> bool {
    // SAFETY: no arguments; the hooks it calls are this module's trampolines (see `Hook`).
    unsafe { ffi::appletMainLoop() }
}

/// The focus state as of the last [`main_loop`].
#[cfg(target_os = "horizon")]
pub fn focus_state() -> FocusState {
    // SAFETY: reads libnx's cached state; no arguments.
    FocusState::from_raw(unsafe { ffi::appletGetFocusState() })
}

#[cfg(target_os = "horizon")]
struct Slot {
    cookie: AppletHookCookie,
    f: Box<dyn FnMut(HookEvent) + Send>,
}

/// A registered applet hook; unhooked on drop. Not `Send`: libnx's hook list has no lock.
#[cfg(target_os = "horizon")]
pub struct Hook(*mut Slot);

#[cfg(target_os = "horizon")]
unsafe extern "C" fn trampoline(hook: u32, param: *mut c_void) {
    if let Some(event) = HookEvent::from_raw(hook) {
        // SAFETY: `param` is the boxed `Slot` registered in `hook`, alive until `Hook` drops and
        // unhooks it; libnx calls hooks from `appletMainLoop` on the one thread that owns it.
        let slot = unsafe { &mut *param.cast::<Slot>() };
        (slot.f)(event);
    }
}

/// Registers `f` for the applet's status changes. `f` runs inside [`main_loop`].
#[cfg(target_os = "horizon")]
pub fn hook(f: impl FnMut(HookEvent) + Send + 'static) -> Hook {
    let slot = Box::into_raw(Box::new(Slot {
        cookie: AppletHookCookie([core::ptr::null_mut(); 3]),
        f: Box::new(f),
    }));
    // SAFETY: `slot` is a live allocation that `Hook` owns until `Drop` unhooks and frees it.
    unsafe { ffi::appletHook(&raw mut (*slot).cookie, trampoline, slot.cast()) };
    Hook(slot)
}

#[cfg(target_os = "horizon")]
impl Drop for Hook {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `Box::into_raw` in `hook`; once unhooked, libnx holds no
        // pointer into it, so the box is freed exactly once here.
        unsafe {
            ffi::appletUnhook(&raw mut (*self.0).cookie);
            drop(Box::from_raw(self.0));
        }
    }
}

/// Holds HOME's exit until [`unlock_exit`]: `main_loop` returns `false` instead, and the system
/// waits 15 s before it terminates the title.
#[cfg(target_os = "horizon")]
pub fn lock_exit() {
    // SAFETY: no arguments.
    let rc = unsafe { ffi::appletLockExit() };
    if rc != 0 {
        tracing::warn!(result = %crate::result_code(rc), "applet exit lock not taken");
    }
}

#[cfg(target_os = "horizon")]
pub fn unlock_exit() {
    // SAFETY: no arguments.
    let rc = unsafe { ffi::appletUnlockExit() };
    if rc != 0 {
        tracing::warn!(result = %crate::result_code(rc), "applet exit lock not released");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_values_follow_applet_h() {
        assert_eq!(OperationMode::from_raw(1), OperationMode::Docked);
        assert_eq!(AppletType::from_raw(2), AppletType::LibraryApplet);
        assert_eq!(AppletType::from_raw(-2), AppletType::Unknown);
        assert_eq!(FocusState::from_raw(3), FocusState::Background);
        assert_eq!(HookEvent::from_raw(3), Some(HookEvent::Exit));
        assert_eq!(HookEvent::from_raw(5), None);
    }
}
