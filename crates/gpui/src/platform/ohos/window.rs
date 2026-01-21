#[cfg(target_env = "ohos")]
use ohos_hilog_binding::{hilog_debug, hilog_info, hilog_warn};

use std::{cell::RefCell, rc::Rc, sync::Arc};

use anyhow::{Result, anyhow};
use futures::channel::oneshot;
use openharmony_ability::{Event, InputEvent, OpenHarmonyApp, Size as OhosSize};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use util::ResultExt;

use std::borrow::Cow;

use crate::platform::blade::{BladeContext, BladeRenderer, BladeSurfaceConfig};
use crate::{
    AnyWindowHandle, Bounds, Capslock, DevicePixels, GpuSpecs, Modifiers, Pixels, PlatformAtlas,
    PlatformDisplay, PlatformInput, PlatformInputHandler, PlatformWindow, Point, PromptButton,
    PromptLevel, RequestFrameOptions, ResizeEdge, Scene, Size, WindowAppearance,
    WindowBackgroundAppearance, WindowBounds, WindowControlArea, WindowControls, WindowDecorations,
    WindowParams, point, px, size,
};
use blade_graphics as gpu;

use super::display::OhosDisplay;

pub(crate) struct OhosWindow {
    app: Rc<RefCell<Option<OpenHarmonyApp>>>,
    handle: AnyWindowHandle,
    bounds: RefCell<Bounds<Pixels>>,
    scale: RefCell<f32>,
    input_handler: RefCell<Option<PlatformInputHandler>>,
    callbacks: RefCell<WindowCallbacks>,
    renderer: RefCell<Option<BladeRenderer>>,
    gpu_context: Arc<BladeContext>,
}

struct WindowCallbacks {
    request_frame: Option<Box<dyn FnMut(RequestFrameOptions)>>,
    input: Option<Box<dyn FnMut(PlatformInput) -> crate::DispatchEventResult>>,
    active_status_change: Option<Box<dyn FnMut(bool)>>,
    hover_status_change: Option<Box<dyn FnMut(bool)>>,
    resize: Option<Box<dyn FnMut(Size<Pixels>, f32)>>,
    moved: Option<Box<dyn FnMut()>>,
    should_close: Option<Box<dyn FnMut() -> bool>>,
    close: Option<Box<dyn FnOnce()>>,
    appearance_changed: Option<Box<dyn FnMut()>>,
    hit_test_window_control: Option<Box<dyn FnMut() -> Option<WindowControlArea>>>,
}

impl OhosWindow {
    pub(crate) fn new(
        app: Rc<RefCell<Option<OpenHarmonyApp>>>,
        handle: AnyWindowHandle,
        params: WindowParams,
        gpu_context: Arc<BladeContext>,
    ) -> Result<Self> {
        let scale = app
            .borrow()
            .as_ref()
            .map(|a| a.scale() as f32)
            .unwrap_or(1.0);
        let bounds = params.bounds;

        // Don't create renderer immediately - native_window may not be available yet.
        // Renderer will be initialized lazily in draw() or when SurfaceCreate event is received.
        // At that point, native_window from OpenHarmonyApp will be available.

        Ok(Self {
            app: app.clone(),
            handle,
            bounds: RefCell::new(bounds),
            scale: RefCell::new(scale),
            input_handler: RefCell::new(None),
            callbacks: RefCell::new(WindowCallbacks {
                request_frame: None,
                input: None,
                active_status_change: None,
                hover_status_change: None,
                resize: None,
                moved: None,
                should_close: None,
                close: None,
                appearance_changed: None,
                hit_test_window_control: None,
            }),
            renderer: RefCell::new(None),
            gpu_context,
        })
    }

    /// Initialize the renderer when native_window becomes available (after SurfaceCreate event).
    /// This method gets the raw_window_handle from OpenHarmonyApp's native_window.
    fn initialize_renderer(&self) -> Result<()> {
        let mut renderer_guard = self.renderer.borrow_mut();
        if renderer_guard.is_some() {
            // Already initialized
            return Ok(());
        }

        // Get native_window from OpenHarmonyApp - it should be available after SurfaceCreate
        let app = self.app.borrow();
        let app_ref = app.as_ref().ok_or_else(|| {
            anyhow::anyhow!("OpenHarmonyApp not available when initializing renderer")
        })?;

        // Check that native_window is available - this is required for the renderer to work.
        // The actual window handle is obtained via HasWindowHandle trait implementation.
        let _native_window = app_ref.native_window().ok_or_else(|| {
            anyhow::anyhow!(
                "native_window not available yet - SurfaceCreate event may not have been received"
            )
        })?;

        // Get the actual window size from content_rect.
        // Using the correct size is important because mismatched sizes between
        // the surface configuration and the actual native_window can cause
        // rendering issues (stretched/cropped content, black borders, etc.)
        // even though create_platform_window_surface itself won't fail.
        let content_rect = app_ref.content_rect();
        let scale = app_ref.scale() as f32;
        let device_width = if content_rect.width > 0 {
            content_rect.width as u32
        } else {
            // Fallback to bounds if content_rect is not available yet
            self.bounds.borrow().size.width.0 as u32
        };
        let device_height = if content_rect.height > 0 {
            content_rect.height as u32
        } else {
            self.bounds.borrow().size.height.0 as u32
        };

        hilog_debug!(
            "OhosWindow: Initializing renderer with size {}x{}",
            device_width,
            device_height
        );

        // Update window bounds to match actual content_rect (convert device px -> logical px)
        if content_rect.width > 0 && content_rect.height > 0 {
            let logical_size = size(
                px(device_width as f32 / scale),
                px(device_height as f32 / scale),
            );
            let logical_origin = point(
                px(content_rect.left as f32 / scale),
                px(content_rect.top as f32 / scale),
            );
            *self.bounds.borrow_mut() = Bounds::new(logical_origin, logical_size);
        }

        let config = BladeSurfaceConfig {
            size: gpu::Extent {
                width: device_width,
                height: device_height,
                depth: 1,
            },
            transparent: true,
        };

        hilog_debug!(
            "OhosWindow: Surface config - width: {}, height: {}, transparent: false",
            device_width,
            device_height
        );

        // Debug: Check window handle before creating renderer
        match self.window_handle() {
            Ok(handle) => {
                hilog_debug!(
                    "OhosWindow: Window handle obtained successfully: {:?}",
                    handle.as_raw()
                );
            }
            Err(e) => {
                hilog_warn!("OhosWindow: Failed to get window handle: {:?}", e);
                return Err(anyhow::anyhow!("Window handle not available: {:?}", e));
            }
        }

        hilog_debug!("OhosWindow: Creating BladeRenderer...");

        // Create renderer using the window's HasWindowHandle and HasDisplayHandle implementation
        // which will get the raw_window_handle from native_window
        let renderer = BladeRenderer::new(&self.gpu_context, self, config)
            .map_err(|e| {
                hilog_warn!("OhosWindow: BladeRenderer::new failed: {}", e);
                anyhow::anyhow!("Failed to create Blade renderer: {}. Make sure native_window is available from OpenHarmonyApp.", e)
            })?;

        *renderer_guard = Some(renderer);
        hilog_debug!("OhosWindow: Renderer initialized successfully");
        Ok(())
    }

    pub(crate) fn handle_event(&self, event: &Event) {
        match event {
            Event::SurfaceCreate { .. } => {
                hilog_debug!("OhosWindow: SurfaceCreate event received - initializing renderer");
                // Initialize renderer when SurfaceCreate event is received
                // Note: on_finish_launching is handled at the platform level (OhosPlatform::handle_ohos_event)
                // before windows are created.
                match self.initialize_renderer() {
                    Ok(()) => {
                        hilog_debug!("OhosWindow: Renderer initialized successfully");
                    }
                    Err(e) => {
                        hilog_warn!(
                            "OhosWindow: Failed to initialize renderer: {}. Make sure native_window is available from OpenHarmonyApp.",
                            e
                        );
                    }
                }
            }
            Event::WindowResize(ohos_size) => {
                let scale = *self.scale.borrow();
                let width = ohos_size.width as f32;
                let height = ohos_size.height as f32;
                let new_size = size(px(width / scale), px(height / scale));
                let origin = self.bounds.borrow().origin;
                *self.bounds.borrow_mut() = Bounds::new(origin, new_size);

                // Update renderer's drawable size
                if let Some(ref mut renderer) = *self.renderer.borrow_mut() {
                    let device_size = Size {
                        width: DevicePixels(width as i32),
                        height: DevicePixels(height as i32),
                    };
                    renderer.update_drawable_size(device_size);
                }

                // Take the callback out to avoid holding borrow during execution
                let mut callback = self.callbacks.borrow_mut().resize.take();
                if let Some(ref mut cb) = callback {
                    cb(new_size, scale);
                }
                // Put it back
                self.callbacks.borrow_mut().resize = callback;
            }
            Event::WindowRedraw { .. } => {
                // Take the callback out to avoid holding borrow during execution
                // This is critical because the callback will eventually call window.draw()
                // which may access other parts of OhosWindow
                let mut callback = self.callbacks.borrow_mut().request_frame.take();
                if let Some(ref mut cb) = callback {
                    cb(RequestFrameOptions {
                        require_presentation: true,
                        force_render: false,
                    });
                } else {
                    hilog_warn!("OhosWindow: WindowRedraw event but no request_frame callback set");
                }
                // Put it back for next frame
                self.callbacks.borrow_mut().request_frame = callback;
            }
            Event::Input(input_event) => {
                self.handle_input_event(input_event);
            }
            Event::GainedFocus => {
                let mut callback = self.callbacks.borrow_mut().active_status_change.take();
                if let Some(ref mut cb) = callback {
                    cb(true);
                }
                self.callbacks.borrow_mut().active_status_change = callback;
            }
            Event::LostFocus => {
                let mut callback = self.callbacks.borrow_mut().active_status_change.take();
                if let Some(ref mut cb) = callback {
                    cb(false);
                }
                self.callbacks.borrow_mut().active_status_change = callback;
            }
            Event::ConfigChanged(..) => {
                let new_scale = self
                    .app
                    .borrow()
                    .as_ref()
                    .map(|a| a.scale() as f32)
                    .unwrap_or(1.0);
                *self.scale.borrow_mut() = new_scale;
                let bounds_size = self.bounds.borrow().size;
                let mut callback = self.callbacks.borrow_mut().resize.take();
                if let Some(ref mut cb) = callback {
                    cb(bounds_size, new_scale);
                }
                self.callbacks.borrow_mut().resize = callback;
            }
            Event::WindowDestroy => {
                // For should_close, we need to call it and check return value
                let mut should_close_callback = self.callbacks.borrow_mut().should_close.take();
                let should_close = if let Some(ref mut cb) = should_close_callback {
                    cb()
                } else {
                    true // Default to allowing close if no callback
                };
                self.callbacks.borrow_mut().should_close = should_close_callback;

                if should_close {
                    // close is FnOnce, so we just take and call it
                    if let Some(callback) = self.callbacks.borrow_mut().close.take() {
                        callback();
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_input_event(&self, event: &InputEvent) {
        // TODO: Convert InputEvent to PlatformInput
        // This is a placeholder implementation
        if let Some(ref mut callback) = self.callbacks.borrow_mut().input {
            // For now, we'll skip input handling as it requires detailed conversion
            // from openharmony_ability::InputEvent to gpui::PlatformInput
        }
    }
}

impl HasWindowHandle for OhosWindow {
    fn window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        if let Some(app) = self.app.borrow().as_ref() {
            if let Some(native_window) = app.native_window() {
                // NOTE: We directly construct OhosNdkWindowHandle from native_window.raw()
                // instead of using native_window.raw_window_handle().
                // This is because the raw_window_handle() method in ohos-xcomponent-binding
                // reads from a global static variable (RAW_WINDOW) instead of using self.raw,
                // which can cause issues if the global variable is not properly synchronized.
                let raw_ptr = native_window.raw();
                if let Some(non_null_ptr) = std::ptr::NonNull::new(raw_ptr) {
                    let ohos_handle = raw_window_handle::OhosNdkWindowHandle::new(non_null_ptr);
                    let raw_handle = raw_window_handle::RawWindowHandle::OhosNdk(ohos_handle);
                    return Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(raw_handle) });
                }
            }
        }
        Err(raw_window_handle::HandleError::Unavailable)
    }
}

impl HasDisplayHandle for OhosWindow {
    fn display_handle(
        &self,
    ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        // Create a dummy display handle for OHOS
        // In a real implementation, this would come from the native window
        let handle = raw_window_handle::OhosDisplayHandle::new();
        let raw_handle = raw_window_handle::RawDisplayHandle::Ohos(handle);
        Ok(unsafe { raw_window_handle::DisplayHandle::borrow_raw(raw_handle) })
    }
}

impl PlatformWindow for OhosWindow {
    fn bounds(&self) -> Bounds<Pixels> {
        *self.bounds.borrow()
    }

    fn is_maximized(&self) -> bool {
        false
    }

    fn window_bounds(&self) -> WindowBounds {
        WindowBounds::Windowed(*self.bounds.borrow())
    }

    fn content_size(&self) -> Size<Pixels> {
        self.bounds.borrow().size
    }

    fn resize(&mut self, size: Size<Pixels>) {
        let origin = self.bounds.borrow().origin;
        *self.bounds.borrow_mut() = Bounds::new(origin, size);
    }

    fn scale_factor(&self) -> f32 {
        *self.scale.borrow()
    }

    fn appearance(&self) -> WindowAppearance {
        WindowAppearance::Light
    }

    fn display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        if let Some(app) = self.app.borrow().clone() {
            Some(Rc::new(OhosDisplay::new(app)))
        } else {
            None
        }
    }

    fn mouse_position(&self) -> Point<Pixels> {
        point(px(0.0), px(0.0))
    }

    fn modifiers(&self) -> Modifiers {
        Modifiers::default()
    }

    fn capslock(&self) -> Capslock {
        Capslock::default()
    }

    fn set_input_handler(&mut self, input_handler: PlatformInputHandler) {
        *self.input_handler.borrow_mut() = Some(input_handler);
    }

    fn take_input_handler(&mut self) -> Option<PlatformInputHandler> {
        self.input_handler.borrow_mut().take()
    }

    fn prompt(
        &self,
        _level: PromptLevel,
        _msg: &str,
        _detail: Option<&str>,
        _answers: &[PromptButton],
    ) -> Option<oneshot::Receiver<usize>> {
        None
    }

    fn activate(&self) {
        // Not supported on OHOS
    }

    fn is_active(&self) -> bool {
        true
    }

    fn is_hovered(&self) -> bool {
        false
    }

    fn set_title(&mut self, _title: &str) {
        // Not supported on OHOS
    }

    fn set_background_appearance(&self, _background_appearance: WindowBackgroundAppearance) {
        // Not supported on OHOS
    }

    fn minimize(&self) {
        // Not supported on OHOS
    }

    fn zoom(&self) {
        // Not supported on OHOS
    }

    fn toggle_fullscreen(&self) {
        // Not supported on OHOS
    }

    fn is_fullscreen(&self) -> bool {
        false
    }

    fn on_request_frame(&self, callback: Box<dyn FnMut(RequestFrameOptions)>) {
        self.callbacks.borrow_mut().request_frame = Some(callback);
    }

    fn on_input(&self, callback: Box<dyn FnMut(PlatformInput) -> crate::DispatchEventResult>) {
        self.callbacks.borrow_mut().input = Some(callback);
    }

    fn on_active_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.callbacks.borrow_mut().active_status_change = Some(callback);
    }

    fn on_hover_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.callbacks.borrow_mut().hover_status_change = Some(callback);
    }

    fn on_resize(&self, callback: Box<dyn FnMut(Size<Pixels>, f32)>) {
        self.callbacks.borrow_mut().resize = Some(callback);
    }

    fn on_moved(&self, callback: Box<dyn FnMut()>) {
        self.callbacks.borrow_mut().moved = Some(callback);
    }

    fn on_should_close(&self, callback: Box<dyn FnMut() -> bool>) {
        self.callbacks.borrow_mut().should_close = Some(callback);
    }

    fn on_hit_test_window_control(&self, callback: Box<dyn FnMut() -> Option<WindowControlArea>>) {
        self.callbacks.borrow_mut().hit_test_window_control = Some(callback);
    }

    fn on_close(&self, callback: Box<dyn FnOnce()>) {
        self.callbacks.borrow_mut().close = Some(callback);
    }

    fn on_appearance_changed(&self, callback: Box<dyn FnMut()>) {
        self.callbacks.borrow_mut().appearance_changed = Some(callback);
    }

    fn draw(&self, scene: &Scene) {
        // Initialize renderer lazily if not already initialized
        // This ensures native_window is available (after SurfaceCreate event)
        if self.renderer.borrow().is_none() {
            if let Err(e) = self.initialize_renderer() {
                hilog_warn!("OhosWindow: Failed to initialize renderer in draw(): {}", e);
                return;
            }
        }

        // Use Blade renderer to render the scene (same as Linux/Wayland)
        if let Some(ref mut renderer) = *self.renderer.borrow_mut() {
            let batch_count = scene.batches().count();
            let bounds = *self.bounds.borrow();
            let scale = *self.scale.borrow();
            hilog_debug!(
                "OhosWindow: draw called with {} batches, bounds: {:?}, scale: {}",
                batch_count,
                bounds,
                scale
            );

            renderer.draw(scene);
        } else {
            hilog_warn!("OhosWindow: draw called but renderer is not available");
        }
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        // Use Blade renderer's atlas, same as Linux Wayland
        if let Some(ref renderer) = *self.renderer.borrow() {
            renderer.sprite_atlas().clone()
        } else {
            // Try to initialize renderer lazily; if it still fails, return dummy atlas with error.
            if self.renderer.borrow().is_none() {
                if let Err(err) = self.initialize_renderer() {
                    hilog_warn!(
                        "OhosWindow: Failed to initialize renderer when fetching atlas: {}",
                        err
                    );
                }
            }
            if let Some(ref renderer) = *self.renderer.borrow() {
                return renderer.sprite_atlas().clone();
            }

            // Fallback to dummy atlas if renderer is not available
            struct DummyAtlas;
            impl PlatformAtlas for DummyAtlas {
                fn get_or_insert_with<'a>(
                    &self,
                    _key: &crate::AtlasKey,
                    _build: &mut dyn FnMut() -> Result<Option<(Size<DevicePixels>, Cow<'a, [u8]>)>>,
                ) -> Result<Option<crate::AtlasTile>> {
                    Err(anyhow!(
                        "renderer not initialized; sprite atlas unavailable"
                    ))
                }
                fn remove(&self, _key: &crate::AtlasKey) {}
            }
            Arc::new(DummyAtlas)
        }
    }

    fn request_decorations(&self, _decorations: WindowDecorations) {
        // Not supported on OHOS
    }

    fn show_window_menu(&self, _position: Point<Pixels>) {
        // Not supported on OHOS
    }

    fn start_window_move(&self) {
        // Not supported on OHOS
    }

    fn start_window_resize(&self, _edge: ResizeEdge) {
        // Not supported on OHOS
    }

    fn window_decorations(&self) -> crate::Decorations {
        crate::Decorations::Server
    }

    fn set_app_id(&mut self, _app_id: &str) {
        // Not supported on OHOS
    }

    fn map_window(&mut self) -> Result<()> {
        Ok(())
    }

    fn window_controls(&self) -> WindowControls {
        WindowControls {
            fullscreen: false,
            maximize: false,
            minimize: false,
            window_menu: false,
        }
    }

    fn set_client_inset(&self, _inset: Pixels) {
        // Not supported on OHOS
    }

    fn gpu_specs(&self) -> Option<GpuSpecs> {
        // Return GPU specs from the Blade renderer
        self.renderer
            .borrow()
            .as_ref()
            .map(|renderer| renderer.gpu_specs())
    }

    fn update_ime_position(&self, _bounds: Bounds<Pixels>) {
        // There is no such thing on Windows.
    }
}
