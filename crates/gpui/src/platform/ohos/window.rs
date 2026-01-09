#[cfg(target_env = "ohos")]
use ohos_hilog_binding::{hilog_debug, hilog_info, hilog_warn};

use std::{cell::RefCell, rc::Rc, sync::Arc};

use anyhow::Result;
use futures::channel::oneshot;
use openharmony_ability::{Event, InputEvent, OpenHarmonyApp, Size as OhosSize};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

use std::borrow::Cow;

use crate::{
    AnyWindowHandle, Bounds, Capslock, DevicePixels, GpuSpecs, Modifiers,
    Pixels, PlatformAtlas, PlatformDisplay, PlatformInput, PlatformInputHandler, PlatformWindow,
    Point, PromptButton, PromptLevel, RequestFrameOptions, ResizeEdge, Scene, Size,
    WindowAppearance, WindowBackgroundAppearance, WindowBounds, WindowControlArea, WindowControls,
    WindowDecorations, WindowParams, point, px, size,
};
use crate::platform::blade::{BladeContext, BladeRenderer, BladeSurfaceConfig};
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
        gpu_context: &BladeContext,
    ) -> Result<Self> {
        let scale = app.borrow().as_ref().map(|a| a.scale() as f32).unwrap_or(1.0);
        let bounds = params.bounds;

        // Create Blade renderer using raw window handle, same as Linux Wayland
        let renderer = {
            // The window implements HasWindowHandle and HasDisplayHandle
            // We'll create a temporary reference for initialization
            let config = BladeSurfaceConfig {
                size: gpu::Extent {
                    width: bounds.size.width.0 as u32,
                    height: bounds.size.height.0 as u32,
                    depth: 1,
                },
                transparent: true,
            };
            
            // We need to create the renderer after the window struct is created
            // So we'll initialize it as None first and create it lazily
            None
        };

        let window = Self {
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
            renderer: RefCell::new(renderer),
        };

        // Initialize renderer now that we have a valid window reference
        let config = BladeSurfaceConfig {
            size: gpu::Extent {
                width: bounds.size.width.0 as u32,
                height: bounds.size.height.0 as u32,
                depth: 1,
            },
            transparent: true,
        };
        
        let renderer = BladeRenderer::new(gpu_context, &window, config)
            .map_err(|e| anyhow::anyhow!("Failed to create Blade renderer: {}", e))?;
        *window.renderer.borrow_mut() = Some(renderer);

        // Event handling is managed by OpenHarmonyApp's run_loop
        // We'll set up callbacks when the window is actually used

        Ok(window)
    }
    

    pub(crate) fn handle_event(&self, event: &Event) {
        hilog_debug!("OhosWindow: Handling event: {:?}", event);

        match event {
            Event::WindowResize(ohos_size) => {
                let width = ohos_size.width as f32;
                let height = ohos_size.height as f32;
                let new_size = size(px(width), px(height));
                *self.bounds.borrow_mut() = Bounds::new(self.bounds.borrow().origin, new_size);
                
                
                if let Some(ref mut callback) = self.callbacks.borrow_mut().resize {
                    callback(new_size, *self.scale.borrow());
                }
            }
            Event::WindowRedraw { .. } => {
                hilog_debug!("OhosWindow: WindowRedraw event - requesting frame");
                if let Some(ref mut callback) = self.callbacks.borrow_mut().request_frame {
                    callback(RequestFrameOptions {
                        require_presentation: true,
                        force_render: false,
                    });
                } else {
                    hilog_warn!("OhosWindow: WindowRedraw event but no request_frame callback set");
                }
            }
            Event::Input(input_event) => {
                self.handle_input_event(input_event);
            }
            Event::GainedFocus => {
                if let Some(ref mut callback) = self.callbacks.borrow_mut().active_status_change {
                    callback(true);
                }
            }
            Event::LostFocus => {
                if let Some(ref mut callback) = self.callbacks.borrow_mut().active_status_change {
                    callback(false);
                }
            }
            Event::ConfigChanged(..) => {
                let new_scale = self.app.borrow().as_ref().map(|a| a.scale() as f32).unwrap_or(1.0);
                *self.scale.borrow_mut() = new_scale;
                if let Some(ref mut callback) = self.callbacks.borrow_mut().resize {
                    callback(self.bounds.borrow().size, new_scale);
                }
            }
            Event::WindowDestroy => {
                if let Some(ref mut callback) = self.callbacks.borrow_mut().should_close {
                    if callback() {
                        if let Some(callback) = self.callbacks.borrow_mut().close.take() {
                            callback();
                        }
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
            if let Some(raw_handle) = native_window.raw_window_handle() {
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
        *self.bounds.borrow_mut() = Bounds::new(self.bounds.borrow().origin, size);
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
        hilog_debug!("OhosWindow: on_request_frame callback set");
        let mut callbacks = self.callbacks.borrow_mut();
        callbacks.request_frame = Some(callback);

        // Request an initial frame to ensure the window renders immediately
        // This is important because on OHOS, we might not receive a WindowRedraw event immediately
        if let Some(ref mut cb) = callbacks.request_frame {
            hilog_debug!("OhosWindow: Requesting initial frame");
            cb(RequestFrameOptions {
                require_presentation: true,
                force_render: true,
            });
        }
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
        // Simple rendering implementation for OHOS
        // Currently, this is a placeholder that logs the draw call
        // In the future, this could be implemented using OHOS native graphics APIs
        // (e.g., using raw-window-handle to get the native window and render directly)
        let batch_count = scene.batches().count();
        hilog_debug!("OhosWindow: draw called with {} batches", batch_count);
        
        // TODO: Implement actual rendering using OHOS native graphics APIs
        // The scene contains all the rendering primitives (paths, quads, sprites, etc.)
        // but we need to convert them to OHOS native graphics calls
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        // Use Blade renderer's atlas, same as Linux Wayland
        if let Some(ref renderer) = *self.renderer.borrow() {
            renderer.sprite_atlas().clone()
        } else {
            // Fallback to dummy atlas if renderer is not available
            struct DummyAtlas;
            impl PlatformAtlas for DummyAtlas {
                fn get_or_insert_with<'a>(
                    &self,
                    _key: &crate::AtlasKey,
                    _build: &mut dyn FnMut() -> Result<Option<(Size<DevicePixels>, Cow<'a, [u8]>)>>,
                ) -> Result<Option<crate::AtlasTile>> {
                    Ok(None)
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
        // GPU specs not available without a renderer
        None
    }

    fn update_ime_position(&self, _bounds: Bounds<Pixels>) {
        // IME position is managed by the system on OHOS
    }
}
