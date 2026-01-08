mod dispatcher;
mod display;
mod keyboard;
mod platform;
mod text_system;
mod window;

pub(crate) use dispatcher::*;
pub(crate) use display::*;
pub(crate) use keyboard::*;
pub(crate) use platform::*;
pub(crate) use text_system::*;
pub(crate) use window::*;

// Re-export set_ohos_app_global for use in app.rs
pub use platform::set_ohos_app_global;

// Re-export set_gpui_app_weak for use in app.rs
pub use platform::set_gpui_app_weak;

pub(crate) type PlatformScreenCaptureFrame = ();

