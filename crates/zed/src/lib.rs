#[cfg(target_env = "ohos")]
include!("main.rs");

#[cfg(target_env = "ohos")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutexattr_setrobust(
    _attr: *mut std::ffi::c_void,
    _robustness: std::ffi::c_int,
) -> std::ffi::c_int {
    0
}

#[cfg(target_env = "ohos")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_consistent(
    _mutex: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    0
}

#[cfg(target_env = "ohos")]
use openharmony_ability_derive::ability;

#[cfg(target_env = "ohos")]
#[ability]
pub fn openharmony_app(app: openharmony_ability::OpenHarmonyApp) {
    run_with_ability_entry(app);
}

#[cfg(not(target_env = "ohos"))]
pub fn openharmony_app_not_available() {}
