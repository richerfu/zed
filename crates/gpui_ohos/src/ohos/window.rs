#[cfg(target_env = "ohos")]
use log::{debug, warn};

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use anyhow::Result;
use futures::channel::oneshot;
use openharmony_ability::{
    ArkUiInputEvent, AvoidAreaType, Event, GestureEvent, GesturePhase, ImeEvent, InputEvent,
    OpenHarmonyApp, PointerInputData, XComponentInputEvent,
    arkui::arkui_input_binding::{UIInputAction, UIInputSourceType, UIInputToolType},
    xcomponent::{MouseAction, MouseButton as OhosMouseButton},
};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

use super::display::OhosDisplay;
use super::wgpu_context::WgpuContext;
use super::wgpu_renderer::{WgpuRenderer, WgpuSurfaceConfig};
use crate::{
    Bounds, Capslock, DevicePixels, ForegroundExecutor, GpuSpecs, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, NavigationDirection, Pixels, PlatformAtlas,
    PlatformDisplay, PlatformInput, PlatformInputHandler, PlatformWindow, Point, PromptButton,
    PromptLevel, RequestFrameOptions, ResizeEdge, Scene, ScrollDelta, ScrollWheelEvent, Size,
    TouchPhase, WindowAppearance, WindowBackgroundAppearance, WindowBounds, WindowControlArea,
    WindowControls, WindowDecorations, WindowParams, point, px, size,
};

const MIN_MOMENTUM_VELOCITY: f32 = 240.0;
const MAX_MOMENTUM_VELOCITY: f32 = 9_000.0;
const MOMENTUM_FRICTION: f32 = 4.2;
const DEFAULT_FRAME_INTERVAL_SECONDS: f32 = 1.0 / 120.0;
const MIN_FRAME_INTERVAL_SECONDS: f32 = 1.0 / 240.0;
const MAX_FRAME_INTERVAL_SECONDS: f32 = 0.05;
const MIN_SCROLL_DELTA: Pixels = px(0.1);

pub(crate) struct OhosWindow {
    app: Rc<RefCell<Option<OpenHarmonyApp>>>,
    bounds: RefCell<Bounds<Pixels>>,
    scale: RefCell<f32>,
    keyboard_overlap_device_px: Cell<i32>,
    safe_area_avoidance_enabled: Cell<bool>,
    input_handler: Rc<RefCell<Option<PlatformInputHandler>>>,
    callbacks: Rc<RefCell<WindowCallbacks>>,
    renderer: RefCell<Option<WgpuRenderer>>,
    gpu_context: Arc<WgpuContext>,
    foreground_executor: ForegroundExecutor,
    keyboard_visible: Rc<Cell<bool>>,
    pointer_position: Cell<Option<Point<Pixels>>>,
    pressed_mouse_button: Cell<Option<MouseButton>>,
    scroll_animation: RefCell<Option<ScrollAnimation>>,
    scroll_frame_rate_boosted: Cell<bool>,
}

pub(crate) struct OhosWindowHandle {
    inner: Rc<RefCell<OhosWindow>>,
    input_handler: Rc<RefCell<Option<PlatformInputHandler>>>,
}

impl OhosWindowHandle {
    pub(crate) fn new(inner: Rc<RefCell<OhosWindow>>) -> Self {
        let input_handler = inner.borrow().input_handler.clone();
        Self {
            inner,
            input_handler,
        }
    }

    fn with_window<R>(&self, f: impl FnOnce(&OhosWindow) -> R) -> R {
        let window = self.inner.borrow();
        f(&window)
    }

    fn with_window_mut<R>(&self, f: impl FnOnce(&mut OhosWindow) -> R) -> R {
        let mut window = self.inner.borrow_mut();
        f(&mut window)
    }
}

struct WindowCallbacks {
    request_frame: Option<Box<dyn FnMut(RequestFrameOptions)>>,
    input: Option<Box<dyn FnMut(PlatformInput) -> crate::DispatchEventResult>>,
    active_status_change: Option<Box<dyn FnMut(bool)>>,
    virtual_keyboard_hidden_by_user: Option<Box<dyn FnMut()>>,
    hover_status_change: Option<Box<dyn FnMut(bool)>>,
    resize: Option<Box<dyn FnMut(Size<Pixels>, f32)>>,
    moved: Option<Box<dyn FnMut()>>,
    should_close: Option<Box<dyn FnMut() -> bool>>,
    close: Option<Box<dyn FnOnce()>>,
    appearance_changed: Option<Box<dyn FnMut()>>,
    hit_test_window_control: Option<Box<dyn FnMut() -> Option<WindowControlArea>>>,
}

#[derive(Clone, Copy)]
struct ScrollAnimation {
    position: Point<Pixels>,
    initial_velocity: Point<f32>,
    elapsed: f32,
    last_distance: Point<Pixels>,
    last_frame_timestamp: Option<Duration>,
}

impl OhosWindow {
    pub(crate) fn new(
        app: Rc<RefCell<Option<OpenHarmonyApp>>>,
        _handle: crate::AnyWindowHandle,
        params: WindowParams,
        gpu_context: Arc<WgpuContext>,
        foreground_executor: ForegroundExecutor,
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
            bounds: RefCell::new(bounds),
            scale: RefCell::new(scale),
            keyboard_overlap_device_px: Cell::new(0),
            safe_area_avoidance_enabled: Cell::new(true),
            input_handler: Rc::new(RefCell::new(None)),
            callbacks: Rc::new(RefCell::new(WindowCallbacks {
                request_frame: None,
                input: None,
                active_status_change: None,
                virtual_keyboard_hidden_by_user: None,
                hover_status_change: None,
                resize: None,
                moved: None,
                should_close: None,
                close: None,
                appearance_changed: None,
                hit_test_window_control: None,
            })),
            renderer: RefCell::new(None),
            gpu_context,
            foreground_executor,
            keyboard_visible: Rc::new(Cell::new(false)),
            pointer_position: Cell::new(None),
            pressed_mouse_button: Cell::new(None),
            scroll_animation: RefCell::new(None),
            scroll_frame_rate_boosted: Cell::new(false),
        })
    }

    fn dispatch_input_with_callbacks(
        callbacks: &Rc<RefCell<WindowCallbacks>>,
        input: PlatformInput,
    ) -> crate::DispatchEventResult {
        let mut callback = callbacks.borrow_mut().input.take();
        let mut result = crate::DispatchEventResult::default();
        if let Some(ref mut cb) = callback {
            result = cb(input);
        }
        callbacks.borrow_mut().input = callback;
        result
    }

    fn dispatch_native_tap(&self, pointer: PointerInputData) {
        if !Self::is_touch_pointer(pointer) || pointer.pointer_count > 1 {
            return;
        }
        let position = self.pointer_position_from_arkui(pointer);
        self.pointer_position.set(Some(position));
        if self.stop_momentum() {
            self.clear_touch_hover_feedback();
            return;
        }

        let modifiers = Modifiers::default();
        self.dispatch_input(PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: 1,
            first_mouse: false,
        }));
        self.dispatch_input(PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: 1,
        }));
        self.clear_touch_hover_feedback();
    }

    fn point_from_device_pixels(&self, x: f32, y: f32) -> Point<Pixels> {
        let scale = (*self.scale.borrow()).max(f32::EPSILON);
        point(px(x / scale), px(y / scale))
    }

    fn velocity_from_device_pixels(&self, x: f32, y: f32) -> Point<f32> {
        let scale = (*self.scale.borrow()).max(f32::EPSILON);
        point(x / scale, y / scale)
    }

    fn pointer_position_from_arkui(&self, pointer: PointerInputData) -> Point<Pixels> {
        self.point_from_device_pixels(pointer.x, pointer.y)
    }

    fn is_touch_pointer(pointer: PointerInputData) -> bool {
        pointer.source_type == UIInputSourceType::TouchScreen
            || matches!(
                pointer.tool_type,
                UIInputToolType::Finger | UIInputToolType::Pen
            )
    }

    fn dispatch_scroll(
        &self,
        position: Point<Pixels>,
        delta: Point<Pixels>,
        touch_phase: TouchPhase,
    ) -> crate::DispatchEventResult {
        Self::dispatch_input_with_callbacks(
            &self.callbacks,
            PlatformInput::ScrollWheel(ScrollWheelEvent {
                position,
                delta: ScrollDelta::Pixels(delta),
                modifiers: Modifiers::default(),
                touch_phase,
            }),
        )
    }

    fn mouse_button(button: OhosMouseButton) -> Option<MouseButton> {
        match button {
            OhosMouseButton::NoneButton => None,
            OhosMouseButton::LeftButton => Some(MouseButton::Left),
            OhosMouseButton::RightButton => Some(MouseButton::Right),
            OhosMouseButton::MiddleButton => Some(MouseButton::Middle),
            OhosMouseButton::BackButton => Some(MouseButton::Navigate(NavigationDirection::Back)),
            OhosMouseButton::ForwardButton => {
                Some(MouseButton::Navigate(NavigationDirection::Forward))
            }
        }
    }

    fn set_scroll_frame_rate_boost(&self, boosted: bool) {
        if self.scroll_frame_rate_boosted.replace(boosted) == boosted {
            return;
        }
        if let Some(app) = self.app.borrow().as_ref() {
            if boosted {
                app.set_frame_rate(60, 120, 120);
            } else {
                app.set_frame_rate(30, 120, 60);
            }
        }
    }

    fn scroll_frame_timestamp(timestamp: i64) -> Option<Duration> {
        u64::try_from(timestamp).ok().map(Duration::from_nanos)
    }

    fn animation_frame_interval(
        animation: &mut ScrollAnimation,
        frame_timestamp: Option<Duration>,
    ) -> f32 {
        let Some(frame_timestamp) = frame_timestamp else {
            return DEFAULT_FRAME_INTERVAL_SECONDS;
        };
        let elapsed = animation
            .last_frame_timestamp
            .and_then(|previous| frame_timestamp.checked_sub(previous))
            .map(|elapsed| elapsed.as_secs_f32())
            .filter(|elapsed| *elapsed > 0.0)
            .unwrap_or(DEFAULT_FRAME_INTERVAL_SECONDS);
        animation.last_frame_timestamp = Some(frame_timestamp);
        elapsed.clamp(MIN_FRAME_INTERVAL_SECONDS, MAX_FRAME_INTERVAL_SECONDS)
    }

    fn scroll_animation_distance(velocity: Point<f32>, elapsed: f32) -> Point<Pixels> {
        let coefficient = (1.0 - (-MOMENTUM_FRICTION * elapsed).exp()) / MOMENTUM_FRICTION;
        point(px(velocity.x * coefficient), px(velocity.y * coefficient))
    }

    fn scroll_animation_velocity(velocity: Point<f32>, elapsed: f32) -> Point<f32> {
        let decay = (-MOMENTUM_FRICTION * elapsed).exp();
        point(velocity.x * decay, velocity.y * decay)
    }

    fn clamp_momentum_velocity(velocity: Point<f32>) -> Point<f32> {
        point(
            velocity
                .x
                .clamp(-MAX_MOMENTUM_VELOCITY, MAX_MOMENTUM_VELOCITY),
            velocity
                .y
                .clamp(-MAX_MOMENTUM_VELOCITY, MAX_MOMENTUM_VELOCITY),
        )
    }

    fn velocity_magnitude(velocity: Point<f32>) -> f32 {
        velocity.x.hypot(velocity.y)
    }

    fn start_momentum(&self, position: Point<Pixels>, velocity: Point<f32>) {
        let velocity = Self::clamp_momentum_velocity(velocity);
        if Self::velocity_magnitude(velocity) < MIN_MOMENTUM_VELOCITY {
            self.dispatch_scroll_end(position);
            return;
        }
        self.set_scroll_frame_rate_boost(true);
        *self.scroll_animation.borrow_mut() = Some(ScrollAnimation {
            position,
            initial_velocity: velocity,
            elapsed: 0.0,
            last_distance: point(px(0.0), px(0.0)),
            last_frame_timestamp: None,
        });
    }

    fn advance_scroll_animation(&self, frame_timestamp: Option<Duration>) {
        let Some(mut animation) = self.scroll_animation.borrow_mut().take() else {
            return;
        };
        let frame_interval = Self::animation_frame_interval(&mut animation, frame_timestamp);
        animation.elapsed += frame_interval;
        let distance =
            Self::scroll_animation_distance(animation.initial_velocity, animation.elapsed);
        let delta = point(
            distance.x - animation.last_distance.x,
            distance.y - animation.last_distance.y,
        );
        animation.last_distance = distance;
        let velocity =
            Self::scroll_animation_velocity(animation.initial_velocity, animation.elapsed);
        if Self::velocity_magnitude(velocity) < MIN_MOMENTUM_VELOCITY
            || (delta.x.abs() < MIN_SCROLL_DELTA && delta.y.abs() < MIN_SCROLL_DELTA)
        {
            self.dispatch_scroll_end(animation.position);
            return;
        }
        let result = self.dispatch_scroll(animation.position, delta, TouchPhase::Moved);
        self.clear_touch_hover_feedback();
        if result.propagate {
            self.dispatch_scroll_end(animation.position);
        } else {
            *self.scroll_animation.borrow_mut() = Some(animation);
        }
    }

    fn dispatch_scroll_end(&self, position: Point<Pixels>) {
        self.scroll_animation.borrow_mut().take();
        self.set_scroll_frame_rate_boost(false);
        self.dispatch_scroll(position, point(px(0.0), px(0.0)), TouchPhase::Ended);
        self.clear_touch_hover_feedback();
    }

    fn stop_momentum(&self) -> bool {
        let position = self
            .scroll_animation
            .borrow_mut()
            .take()
            .map(|animation| animation.position);
        let Some(position) = position else {
            return false;
        };
        self.set_scroll_frame_rate_boost(false);
        self.dispatch_scroll(position, point(px(0.0), px(0.0)), TouchPhase::Ended);
        true
    }

    fn clear_touch_hover_feedback(&self) {
        self.dispatch_input(PlatformInput::MouseMove(MouseMoveEvent {
            position: point(px(-1.0), px(-1.0)),
            pressed_button: None,
            modifiers: Modifiers::default(),
        }));
    }

    fn show_keyboard_if_needed(&self) {
        if !self.keyboard_visible.get() {
            if let Some(app) = self.app.borrow().as_ref() {
                app.show_keyboard();
                self.keyboard_visible.set(true);
            }
        }
    }

    fn hide_keyboard_if_needed(&self) {
        if self.keyboard_visible.replace(false) {
            if let Some(app) = self.app.borrow().as_ref() {
                app.hide_keyboard();
            }
        }
    }

    fn notify_keyboard_hidden_by_user_if_needed(&self) {
        if self.keyboard_visible.replace(false) {
            let mut callback = self
                .callbacks
                .borrow_mut()
                .virtual_keyboard_hidden_by_user
                .take();
            if let Some(ref mut cb) = callback {
                cb();
            }
            self.callbacks.borrow_mut().virtual_keyboard_hidden_by_user = callback;
        }
    }

    fn keyboard_inset_for_overlap(&self, overlap_device_px: i32) -> Pixels {
        const MIN_CONTENT_HEIGHT: f32 = 64.0;

        let overlap = overlap_device_px.max(0) as f32;
        let scale = self.scale_factor().max(1.0);
        let mut inset = (overlap / scale).max(0.0);
        let bounds_height = self.bounds.borrow().size.height.as_f32().max(0.0);
        let max_inset = (bounds_height - MIN_CONTENT_HEIGHT).max(0.0);
        if inset > max_inset {
            inset = max_inset;
        }
        px(inset)
    }

    fn keyboard_overlap_from_avoid_area_device_px(&self) -> Option<i32> {
        if !self.safe_area_avoidance_enabled.get() {
            return Some(0);
        }

        let app_ref = self.app.borrow();
        let app = app_ref.as_ref()?;

        let content_rect = app.content_rect();
        if content_rect.height <= 0 {
            return Some(0);
        }

        // Use actual XComponent rect as layout basis for keyboard-avoid computation.
        // This keeps behavior correct for embedded/non-fullscreen XComponents.
        let layout_top = content_rect.top;
        let layout_height = content_rect.height.max(0);
        if layout_height <= 0 {
            return Some(0);
        }
        let window_rect = app.window_rect();
        let window_top = window_rect.top;
        let window_bottom = window_rect.top.saturating_add(window_rect.height.max(0));

        let keyboard_area = app.avoid_area(AvoidAreaType::Keyboard);
        let system_area = app.avoid_area(AvoidAreaType::System);
        let system_gesture_area = app.avoid_area(AvoidAreaType::SystemGesture);
        let navigation_indicator_area = app.avoid_area(AvoidAreaType::NavigationIndicator);

        // OHOS avoid-area bottomRect coordinates are in window/screen space.
        // XComponent's content_rect can be reported in safe-content coordinates on some devices.
        // For root full-width layouts, infer top-safe offset so intersection uses a consistent space.
        let root_layout_width_matches_window = content_rect.width > 0
            && window_rect.width > 0
            && (content_rect.width - window_rect.width).abs() <= 1;
        let can_infer_root_safe_top = layout_top == 0
            && layout_height > 0
            && window_rect.height >= layout_height
            && root_layout_width_matches_window;
        let inferred_outside_bottom_safe = if can_infer_root_safe_top {
            let bottom_safe_overlap = |area: Option<openharmony_ability::AvoidArea>| -> i32 {
                let Some(area) = area else {
                    return 0;
                };
                if !area.visible || area.bottom_rect.height <= 0 {
                    return 0;
                }
                let start = area.bottom_rect.top;
                let end = area
                    .bottom_rect
                    .top
                    .saturating_add(area.bottom_rect.height.max(0));
                if end < window_bottom {
                    return 0;
                }
                (window_bottom - start)
                    .max(0)
                    .min(area.bottom_rect.height.max(0))
            };

            bottom_safe_overlap(system_area)
                .max(bottom_safe_overlap(system_gesture_area))
                .max(bottom_safe_overlap(navigation_indicator_area))
        } else {
            0
        };
        let inferred_top_safe = if can_infer_root_safe_top {
            (window_rect.height.max(0) - layout_height - inferred_outside_bottom_safe).max(0)
        } else {
            0
        };
        // Convert GPUI layout bounds to screen space before intersection.
        let layout_top_screen = window_top
            .saturating_add(inferred_top_safe)
            .saturating_add(layout_top);
        let layout_bottom_screen = layout_top_screen.saturating_add(layout_height);

        let keyboard_avoid_visible = keyboard_area.map(|a| a.visible).unwrap_or(false);
        if !(self.keyboard_visible.get() || keyboard_avoid_visible) {
            return Some(0);
        }

        // Keyboard event only determines show/hide state.
        // Actual inset is derived from avoid-area geometry.
        // When keyboard is shown, include bottom occlusion union of:
        // - Keyboard area
        // - System bottom area (3-button navigation etc.)
        // - System gesture area
        // - Navigation indicator area
        // This prevents under-subtraction where keyboard area excludes nav area.
        let mut intervals: Vec<(i32, i32)> = Vec::with_capacity(4);
        let mut push_bottom_overlap_interval =
            |area: openharmony_ability::AvoidArea, require_visible: bool| {
                if area.bottom_rect.height <= 0 {
                    return;
                }
                if require_visible && !area.visible {
                    return;
                }
                let start = area.bottom_rect.top.max(layout_top_screen);
                let end = area
                    .bottom_rect
                    .top
                    .saturating_add(area.bottom_rect.height.max(0))
                    .min(layout_bottom_screen);
                if end > start {
                    intervals.push((start, end));
                }
            };

        if let Some(area) = keyboard_area {
            push_bottom_overlap_interval(area, true);
        }
        if let Some(area) = system_area {
            push_bottom_overlap_interval(area, false);
        }
        if let Some(area) = system_gesture_area {
            push_bottom_overlap_interval(area, false);
        }
        if let Some(area) = navigation_indicator_area {
            push_bottom_overlap_interval(area, false);
        }

        if intervals.is_empty() {
            return Some(0);
        }

        intervals.sort_unstable_by_key(|(start, _)| *start);
        let mut union_overlap = 0i32;
        let mut current = intervals[0];
        for &(start, end) in intervals.iter().skip(1) {
            if start <= current.1 {
                current.1 = current.1.max(end);
            } else {
                union_overlap = union_overlap.saturating_add(current.1 - current.0);
                current = (start, end);
            }
        }
        union_overlap = union_overlap.saturating_add(current.1 - current.0);

        let geometric_overlap = union_overlap.min(layout_height.max(0));
        let clamped_overlap = geometric_overlap;

        Some(clamped_overlap)
    }

    fn set_safe_area_avoidance_enabled(&self, enabled: bool) {
        let previous = self.safe_area_avoidance_enabled.replace(enabled);
        if previous != enabled && self.refresh_keyboard_overlap_device_px() {
            self.emit_resize_callback();
        }
    }

    fn refresh_keyboard_overlap_device_px(&self) -> bool {
        let previous_overlap = self.keyboard_overlap_device_px.get();
        let next_overlap = self
            .keyboard_overlap_from_avoid_area_device_px()
            .unwrap_or(0)
            .max(0);
        if previous_overlap != next_overlap {
            self.keyboard_overlap_device_px.set(next_overlap);
            true
        } else {
            false
        }
    }

    fn effective_content_size(&self) -> Size<Pixels> {
        let bounds_size = self.bounds.borrow().size;
        let bounds_height = bounds_size.height.as_f32().max(0.0);
        let keyboard_inset = self
            .keyboard_inset_for_overlap(self.keyboard_overlap_device_px.get())
            .as_f32();
        size(
            bounds_size.width,
            px((bounds_height - keyboard_inset).max(0.0)),
        )
    }

    fn emit_resize_callback(&self) {
        let scale = *self.scale.borrow();
        let content_size = self.effective_content_size();

        let mut callback = self.callbacks.borrow_mut().resize.take();
        if let Some(ref mut cb) = callback {
            cb(content_size, scale);
        }
        self.callbacks.borrow_mut().resize = callback;
    }

    /// Initialize the renderer when native_window becomes available (after SurfaceCreate event).
    /// This method gets the raw_window_handle from OpenHarmonyApp's native_window.
    pub(crate) fn initialize_renderer(&self) -> Result<()> {
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
            self.bounds.borrow().size.width.as_f32() as u32
        };
        let device_height = if content_rect.height > 0 {
            content_rect.height as u32
        } else {
            self.bounds.borrow().size.height.as_f32() as u32
        };

        debug!(
            "OhosWindow: Initializing renderer with size {}x{}",
            device_width, device_height
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

        let config = WgpuSurfaceConfig {
            size: Size {
                width: DevicePixels(device_width as i32),
                height: DevicePixels(device_height as i32),
            },
            transparent: true,
        };

        debug!(
            "OhosWindow: Surface config - width: {}, height: {}, transparent: false",
            device_width, device_height
        );

        // Debug: Check window handle before creating renderer
        match self.window_handle() {
            Ok(handle) => {
                debug!(
                    "OhosWindow: Window handle obtained successfully: {:?}",
                    handle.as_raw()
                );
            }
            Err(e) => {
                warn!("OhosWindow: Failed to get window handle: {:?}", e);
                return Err(anyhow::anyhow!("Window handle not available: {:?}", e));
            }
        }

        debug!("OhosWindow: Creating WgpuRenderer...");

        // Create renderer using the window's HasWindowHandle and HasDisplayHandle implementation
        // which will get the raw_window_handle from native_window
        let renderer = WgpuRenderer::new(&self.gpu_context, self, config)
            .map_err(|e| {
                warn!("OhosWindow: WgpuRenderer::new failed: {}", e);
                anyhow::anyhow!("Failed to create Wgpu renderer: {}. Make sure native_window is available from OpenHarmonyApp.", e)
            })?;

        *renderer_guard = Some(renderer);
        debug!("OhosWindow: Renderer initialized successfully");
        Ok(())
    }

    pub(crate) fn handle_event(&self, event: &Event) {
        match event {
            Event::SurfaceCreate => {
                debug!("OhosWindow: SurfaceCreate event received - initializing renderer");
                // Initialize renderer when SurfaceCreate event is received
                // Note: on_finish_launching is handled at the platform level (OhosPlatform::handle_ohos_event)
                // before windows are created.
                match self.initialize_renderer() {
                    Ok(()) => {
                        debug!("OhosWindow: Renderer initialized successfully");
                    }
                    Err(e) => {
                        warn!(
                            "OhosWindow: Failed to initialize renderer: {}. Make sure native_window is available from OpenHarmonyApp.",
                            e
                        );
                    }
                }
                if self.refresh_keyboard_overlap_device_px() {
                    self.emit_resize_callback();
                }
            }
            Event::WindowResize(ohos_size) => {
                let scale = *self.scale.borrow();
                let width = ohos_size.width as f32;
                let height = ohos_size.height as f32;
                let new_size = size(px(width / scale), px(height / scale));
                let origin = self.bounds.borrow().origin;
                *self.bounds.borrow_mut() = Bounds::new(origin, new_size);
                self.refresh_keyboard_overlap_device_px();

                // Update renderer's drawable size
                if let Some(ref mut renderer) = *self.renderer.borrow_mut() {
                    let device_size = Size {
                        width: DevicePixels(width as i32),
                        height: DevicePixels(height as i32),
                    };
                    renderer.update_drawable_size(device_size);
                }
                self.emit_resize_callback();
            }
            Event::ContentRectChange(..) => {
                if self.refresh_keyboard_overlap_device_px() {
                    self.emit_resize_callback();
                }
            }
            Event::AvoidAreaChange(info) => {
                if matches!(
                    info.area_type,
                    AvoidAreaType::Keyboard
                        | AvoidAreaType::System
                        | AvoidAreaType::SystemGesture
                        | AvoidAreaType::NavigationIndicator
                ) && self.refresh_keyboard_overlap_device_px()
                {
                    self.emit_resize_callback();
                }
            }
            Event::WindowRedraw(info) => {
                self.advance_scroll_animation(
                    Self::scroll_frame_timestamp(info.target_time_stamp)
                        .or_else(|| Self::scroll_frame_timestamp(info.time_stamp)),
                );
                // Take the callback out to avoid holding borrow during execution
                // This is critical because the callback will eventually call window.draw()
                // which may access other parts of OhosWindow
                let mut callback = self.callbacks.borrow_mut().request_frame.take();
                if let Some(ref mut cb) = callback {
                    cb(RequestFrameOptions {
                        require_presentation: false,
                        force_render: false,
                    });
                } else {
                    warn!("OhosWindow: WindowRedraw event but no request_frame callback set");
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
                self.stop_momentum();
                let mut callback = self.callbacks.borrow_mut().active_status_change.take();
                if let Some(ref mut cb) = callback {
                    cb(false);
                }
                self.callbacks.borrow_mut().active_status_change = callback;
                self.hide_keyboard_if_needed();
                if self.refresh_keyboard_overlap_device_px() {
                    self.emit_resize_callback();
                }
            }
            Event::ConfigChanged(..) => {
                let new_scale = self
                    .app
                    .borrow()
                    .as_ref()
                    .map(|a| a.scale() as f32)
                    .unwrap_or(1.0);
                *self.scale.borrow_mut() = new_scale;
                self.refresh_keyboard_overlap_device_px();
                self.emit_resize_callback();
            }
            Event::WindowDestroy => {
                self.stop_momentum();
                if self.refresh_keyboard_overlap_device_px() {
                    self.emit_resize_callback();
                }
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
            Event::KeyboardEvent(height) => {
                if *height <= 0 {
                    self.notify_keyboard_hidden_by_user_if_needed();
                } else {
                    self.keyboard_visible.set(true);
                }
                if self.refresh_keyboard_overlap_device_px() {
                    self.emit_resize_callback();
                }
            }
            _ => {}
        }
    }

    fn handle_input_event(&self, event: &InputEvent) {
        match event {
            InputEvent::Ime(ime_event) => {
                if matches!(
                    ime_event,
                    ImeEvent::ImeStatusEvent(openharmony_ability::ime::KeyboardStatus::Hide)
                ) {
                    self.notify_keyboard_hidden_by_user_if_needed();
                    if self.refresh_keyboard_overlap_device_px() {
                        self.emit_resize_callback();
                    }
                }

                let handler_ref = self.input_handler.clone();
                let ime_event = ime_event.clone();
                let executor = self.foreground_executor.clone();

                executor
                    .spawn(async move {
                        let mut handler_guard = handler_ref.borrow_mut();
                        let Some(handler) = handler_guard.as_mut() else {
                            return;
                        };

                        match ime_event {
                            ImeEvent::TextInputEvent(data) => {
                                handler.replace_text_in_range(None, &data.text);
                                handler.unmark_text();
                            }
                            ImeEvent::EnterEvent(_action) => {
                                handler.replace_text_in_range(None, "\n");
                                handler.unmark_text();
                            }
                            ImeEvent::BackspaceEvent(len) => {
                                let len = (len).max(0) as usize;
                                if len == 0 {
                                    return;
                                }

                                if let Some(selection) = handler.selected_text_range(true) {
                                    let range = if selection.range.start != selection.range.end {
                                        selection.range
                                    } else {
                                        let caret = if selection.reversed {
                                            selection.range.start
                                        } else {
                                            selection.range.end
                                        };
                                        let start = caret.saturating_sub(len);
                                        start..caret
                                    };
                                    handler.replace_text_in_range(Some(range), "");
                                } else {
                                    handler.replace_text_in_range(None, "");
                                }
                            }
                            ImeEvent::ImeStatusEvent(status) => {
                                if matches!(status, openharmony_ability::ime::KeyboardStatus::Hide)
                                {
                                    handler.unmark_text();
                                }
                            }
                        }
                    })
                    .detach();
            }
            InputEvent::XComponent(XComponentInputEvent::Mouse(mouse_event)) => {
                let position = self.point_from_device_pixels(mouse_event.x, mouse_event.y);
                self.pointer_position.set(Some(position));
                let event_button = Self::mouse_button(mouse_event.button);
                match mouse_event.action {
                    MouseAction::Press => {
                        let Some(button) = event_button else {
                            return;
                        };
                        self.pressed_mouse_button.set(Some(button));
                        self.dispatch_input(PlatformInput::MouseDown(MouseDownEvent {
                            button,
                            position,
                            modifiers: Modifiers::default(),
                            click_count: 1,
                            first_mouse: false,
                        }));
                    }
                    MouseAction::Release => {
                        let button = event_button.or(self.pressed_mouse_button.get());
                        self.pressed_mouse_button.set(None);
                        let Some(button) = button else {
                            return;
                        };
                        self.dispatch_input(PlatformInput::MouseUp(MouseUpEvent {
                            button,
                            position,
                            modifiers: Modifiers::default(),
                            click_count: 1,
                        }));
                    }
                    MouseAction::Move => {
                        self.dispatch_input(PlatformInput::MouseMove(MouseMoveEvent {
                            position,
                            pressed_button: self.pressed_mouse_button.get().or(event_button),
                            modifiers: Modifiers::default(),
                        }));
                    }
                    MouseAction::None => {}
                }
            }
            InputEvent::ArkUi(ArkUiInputEvent::Axis(axis_event)) => {
                self.stop_momentum();
                let position = self.pointer_position_from_arkui(axis_event.pointer);
                self.pointer_position.set(Some(position));
                let raw_delta = point(axis_event.delta_x as f32, axis_event.delta_y as f32);
                let delta = if axis_event.pointer.tool_type == UIInputToolType::Touchpad {
                    self.point_from_device_pixels(raw_delta.x, raw_delta.y)
                } else {
                    raw_delta.map(px)
                };
                let touch_phase = match axis_event.pointer.action {
                    UIInputAction::Down => TouchPhase::Started,
                    UIInputAction::Up | UIInputAction::Cancel => TouchPhase::Ended,
                    UIInputAction::Move => TouchPhase::Moved,
                };
                if delta.x != px(0.0)
                    || delta.y != px(0.0)
                    || matches!(touch_phase, TouchPhase::Started | TouchPhase::Ended)
                {
                    self.dispatch_scroll(position, delta, touch_phase);
                }
            }
            InputEvent::ArkUi(ArkUiInputEvent::Gesture(gesture_event)) => match gesture_event {
                GestureEvent::Tap(tap_event) => self.dispatch_native_tap(tap_event.pointer),
                GestureEvent::Pan(pan_event) => {
                    if !Self::is_touch_pointer(pan_event.pointer) {
                        return;
                    }
                    let position = self.pointer_position_from_arkui(pan_event.pointer);
                    self.pointer_position.set(Some(position));
                    if pan_event.pointer.pointer_count > 1 {
                        self.dispatch_scroll_end(position);
                        return;
                    }
                    let delta = self.point_from_device_pixels(pan_event.delta_x, pan_event.delta_y);
                    match pan_event.phase {
                        GesturePhase::Start => {
                            self.stop_momentum();
                            self.dispatch_scroll(position, delta, TouchPhase::Started);
                        }
                        GesturePhase::Update => {
                            self.dispatch_scroll(position, delta, TouchPhase::Moved);
                        }
                        GesturePhase::End => {
                            if delta.x != px(0.0) || delta.y != px(0.0) {
                                self.dispatch_scroll(position, delta, TouchPhase::Moved);
                            }
                            self.start_momentum(
                                position,
                                self.velocity_from_device_pixels(
                                    pan_event.velocity_x,
                                    pan_event.velocity_y,
                                ),
                            );
                        }
                        GesturePhase::Cancel => {
                            self.dispatch_scroll_end(position);
                        }
                    }
                    self.clear_touch_hover_feedback();
                }
                GestureEvent::Swipe(..) => {}
            },
            InputEvent::XComponent(
                XComponentInputEvent::Key(_) | XComponentInputEvent::Touch(_),
            ) => {}
        }
    }

    fn dispatch_input(&self, input: PlatformInput) {
        Self::dispatch_input_with_callbacks(&self.callbacks, input);
    }
}

impl HasWindowHandle for OhosWindow {
    fn window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        self.app
            .borrow()
            .as_ref()
            .and_then(|app| app.native_window())
            .and_then(|native_window| native_window.raw_window_handle())
            .map(|raw_handle| unsafe { raw_window_handle::WindowHandle::borrow_raw(raw_handle) })
            .ok_or(raw_window_handle::HandleError::Unavailable)
    }
}

impl HasWindowHandle for OhosWindowHandle {
    fn window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        self.inner
            .borrow()
            .app
            .borrow()
            .as_ref()
            .and_then(|app| app.native_window())
            .and_then(|native_window| native_window.raw_window_handle())
            .map(|raw_handle| unsafe { raw_window_handle::WindowHandle::borrow_raw(raw_handle) })
            .ok_or(raw_window_handle::HandleError::Unavailable)
    }
}

impl HasDisplayHandle for OhosWindow {
    fn display_handle(
        &self,
    ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        Ok(raw_window_handle::DisplayHandle::ohos())
    }
}

impl HasDisplayHandle for OhosWindowHandle {
    fn display_handle(
        &self,
    ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        Ok(raw_window_handle::DisplayHandle::ohos())
    }
}

impl PlatformWindow for OhosWindowHandle {
    fn bounds(&self) -> Bounds<Pixels> {
        self.with_window(|window| window.bounds())
    }

    fn is_maximized(&self) -> bool {
        self.with_window(|window| window.is_maximized())
    }

    fn window_bounds(&self) -> WindowBounds {
        self.with_window(|window| window.window_bounds())
    }

    fn content_size(&self) -> Size<Pixels> {
        self.with_window(|window| window.content_size())
    }

    fn resize(&mut self, size: Size<Pixels>) {
        self.with_window_mut(|window| window.resize(size))
    }

    fn scale_factor(&self) -> f32 {
        self.with_window(|window| window.scale_factor())
    }

    fn appearance(&self) -> WindowAppearance {
        self.with_window(|window| window.appearance())
    }

    fn display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        self.with_window(|window| window.display())
    }

    fn mouse_position(&self) -> Point<Pixels> {
        self.with_window(|window| window.mouse_position())
    }

    fn modifiers(&self) -> Modifiers {
        self.with_window(|window| window.modifiers())
    }

    fn capslock(&self) -> Capslock {
        self.with_window(|window| window.capslock())
    }

    fn set_input_handler(&mut self, input_handler: PlatformInputHandler) {
        *self.input_handler.borrow_mut() = Some(input_handler);
    }

    fn take_input_handler(&mut self) -> Option<PlatformInputHandler> {
        self.input_handler.borrow_mut().take()
    }

    fn prompt(
        &self,
        level: PromptLevel,
        msg: &str,
        detail: Option<&str>,
        answers: &[PromptButton],
    ) -> Option<oneshot::Receiver<usize>> {
        self.with_window(|window| window.prompt(level, msg, detail, answers))
    }

    fn activate(&self) {
        self.with_window(|window| window.activate())
    }

    fn is_active(&self) -> bool {
        self.with_window(|window| window.is_active())
    }

    fn is_hovered(&self) -> bool {
        self.with_window(|window| window.is_hovered())
    }

    fn background_appearance(&self) -> WindowBackgroundAppearance {
        self.with_window(|window| window.background_appearance())
    }

    fn set_title(&mut self, title: &str) {
        self.with_window_mut(|window| window.set_title(title))
    }

    fn set_background_appearance(&self, background_appearance: WindowBackgroundAppearance) {
        self.with_window(|window| window.set_background_appearance(background_appearance))
    }

    fn minimize(&self) {
        self.with_window(|window| window.minimize())
    }

    fn zoom(&self) {
        self.with_window(|window| window.zoom())
    }

    fn toggle_fullscreen(&self) {
        self.with_window(|window| window.toggle_fullscreen())
    }

    fn is_fullscreen(&self) -> bool {
        self.with_window(|window| window.is_fullscreen())
    }

    fn on_request_frame(&self, callback: Box<dyn FnMut(RequestFrameOptions)>) {
        self.with_window(|window| window.on_request_frame(callback))
    }

    fn on_input(&self, callback: Box<dyn FnMut(PlatformInput) -> crate::DispatchEventResult>) {
        self.with_window(|window| window.on_input(callback))
    }

    fn on_active_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.with_window(|window| window.on_active_status_change(callback))
    }

    fn on_virtual_keyboard_hidden_by_user(&self, callback: Box<dyn FnMut()>) {
        self.with_window(|window| {
            window
                .callbacks
                .borrow_mut()
                .virtual_keyboard_hidden_by_user = Some(callback)
        })
    }

    fn on_hover_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.with_window(|window| window.on_hover_status_change(callback))
    }

    fn set_virtual_keyboard_visible(&self, visible: bool) {
        self.with_window(|window| {
            if visible {
                window.show_keyboard_if_needed();
            } else {
                window.hide_keyboard_if_needed();
            }
        })
    }

    fn set_safe_area_avoidance(&self, enabled: bool) {
        self.with_window(|window| window.set_safe_area_avoidance_enabled(enabled))
    }

    fn on_resize(&self, callback: Box<dyn FnMut(Size<Pixels>, f32)>) {
        self.with_window(|window| window.on_resize(callback))
    }

    fn on_moved(&self, callback: Box<dyn FnMut()>) {
        self.with_window(|window| window.on_moved(callback))
    }

    fn on_should_close(&self, callback: Box<dyn FnMut() -> bool>) {
        self.with_window(|window| window.on_should_close(callback))
    }

    fn on_hit_test_window_control(&self, callback: Box<dyn FnMut() -> Option<WindowControlArea>>) {
        self.with_window(|window| window.on_hit_test_window_control(callback))
    }

    fn on_close(&self, callback: Box<dyn FnOnce()>) {
        self.with_window(|window| window.on_close(callback))
    }

    fn on_appearance_changed(&self, callback: Box<dyn FnMut()>) {
        self.with_window(|window| window.on_appearance_changed(callback))
    }

    fn draw(&self, scene: &Scene) {
        self.with_window(|window| window.draw(scene))
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        self.with_window(|window| window.sprite_atlas())
    }

    fn gpu_specs(&self) -> Option<GpuSpecs> {
        self.with_window(|window| window.gpu_specs())
    }

    fn is_subpixel_rendering_supported(&self) -> bool {
        self.with_window(|window| window.is_subpixel_rendering_supported())
    }

    fn update_ime_position(&self, bounds: Bounds<Pixels>) {
        self.with_window(|window| window.update_ime_position(bounds))
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
        self.effective_content_size()
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
        self.pointer_position
            .get()
            .unwrap_or_else(|| point(px(0.0), px(0.0)))
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

    fn background_appearance(&self) -> WindowBackgroundAppearance {
        WindowBackgroundAppearance::Opaque
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

    fn on_virtual_keyboard_hidden_by_user(&self, callback: Box<dyn FnMut()>) {
        self.callbacks.borrow_mut().virtual_keyboard_hidden_by_user = Some(callback);
    }

    fn on_hover_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.callbacks.borrow_mut().hover_status_change = Some(callback);
    }

    fn set_virtual_keyboard_visible(&self, visible: bool) {
        if visible {
            self.show_keyboard_if_needed();
        } else {
            self.hide_keyboard_if_needed();
        }
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
                warn!("OhosWindow: Failed to initialize renderer in draw(): {}", e);
                return;
            }
        }

        // Use WGPU renderer to render the scene.
        if let Some(ref mut renderer) = *self.renderer.borrow_mut() {
            renderer.draw(scene);
        } else {
            warn!("OhosWindow: draw called but renderer is not available");
        }
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        if let Some(ref renderer) = *self.renderer.borrow() {
            renderer.sprite_atlas().clone()
        } else {
            if let Err(error) = self.initialize_renderer() {
                panic!("OhosWindow: renderer must be initialized before sprite_atlas: {error}");
            }
            self.renderer
                .borrow()
                .as_ref()
                .expect("renderer should be initialized after initialize_renderer")
                .sprite_atlas()
                .clone()
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
        // Keyboard avoidance is driven by content_size updates from avoid-area overlap.
        // client_inset is intentionally ignored on OHOS.
    }

    fn set_safe_area_avoidance(&self, enabled: bool) {
        self.set_safe_area_avoidance_enabled(enabled);
    }

    fn gpu_specs(&self) -> Option<GpuSpecs> {
        // Return GPU specs from the WGPU renderer.
        self.renderer
            .borrow()
            .as_ref()
            .map(|renderer| renderer.gpu_specs())
    }

    fn is_subpixel_rendering_supported(&self) -> bool {
        false
    }

    fn update_ime_position(&self, _bounds: Bounds<Pixels>) {
        // There is no such thing on Windows.
    }
}

#[cfg(test)]
mod tests {
    use openharmony_ability::arkui::arkui_input_binding::{
        UIInputAction, UIInputEvent, UIInputSourceType, UIInputToolType,
    };

    use super::OhosWindow;
    use crate::point;

    #[test]
    fn momentum_distance_increases_and_velocity_decays() {
        let velocity = point(1_000.0, -500.0);
        let early_distance = OhosWindow::scroll_animation_distance(velocity, 0.1);
        let late_distance = OhosWindow::scroll_animation_distance(velocity, 0.5);
        let early_velocity = OhosWindow::scroll_animation_velocity(velocity, 0.1);
        let late_velocity = OhosWindow::scroll_animation_velocity(velocity, 0.5);

        assert!(late_distance.x > early_distance.x);
        assert!(late_distance.y < early_distance.y);
        assert!(late_velocity.x.abs() < early_velocity.x.abs());
        assert!(late_velocity.y.abs() < early_velocity.y.abs());
    }

    #[test]
    fn momentum_velocity_is_clamped_per_axis() {
        let velocity = OhosWindow::clamp_momentum_velocity(point(20_000.0, -20_000.0));
        assert_eq!(velocity, point(9_000.0, -9_000.0));
    }

    #[test]
    fn arkui_touch_filter_rejects_mouse_gestures() {
        assert!(OhosWindow::is_touch_pointer(pointer(
            UIInputSourceType::TouchScreen,
            UIInputToolType::Finger,
        )));
        assert!(OhosWindow::is_touch_pointer(pointer(
            UIInputSourceType::Unknown,
            UIInputToolType::Pen,
        )));
        assert!(!OhosWindow::is_touch_pointer(pointer(
            UIInputSourceType::Mouse,
            UIInputToolType::Mouse,
        )));
    }

    fn pointer(
        source_type: UIInputSourceType,
        tool_type: UIInputToolType,
    ) -> openharmony_ability::PointerInputData {
        openharmony_ability::PointerInputData {
            event_type: UIInputEvent::Touch,
            action: UIInputAction::Move,
            source_type,
            tool_type,
            x: 10.0,
            y: 20.0,
            window_x: 10.0,
            window_y: 20.0,
            display_x: 10.0,
            display_y: 20.0,
            timestamp: 42,
            pointer_count: 1,
            pointer_id: Some(0),
        }
    }
}
