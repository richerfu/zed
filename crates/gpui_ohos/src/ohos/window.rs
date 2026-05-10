#[cfg(target_env = "ohos")]
use log::{debug, warn};

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use anyhow::Result;
use futures::channel::oneshot;
use openharmony_ability::{
    AvoidAreaType, Event, ImeEvent, InputEvent, OpenHarmonyApp, xcomponent::TouchEvent,
    xcomponent::TouchEventData,
};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

use super::display::OhosDisplay;
use super::wgpu_context::WgpuContext;
use super::wgpu_renderer::{WgpuRenderer, WgpuSurfaceConfig};
use crate::{
    Axis, Bounds, Capslock, DevicePixels, ForegroundExecutor, GpuSpecs, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, PlatformAtlas, PlatformDisplay,
    PlatformInput, PlatformInputHandler, PlatformWindow, Point, PromptButton, PromptLevel,
    RequestFrameOptions, ResizeEdge, Scene, ScrollDelta, ScrollWheelEvent, Size, TouchPhase,
    WindowAppearance, WindowBackgroundAppearance, WindowBounds, WindowControlArea, WindowControls,
    WindowDecorations, WindowParams, point, px, size,
};

pub(crate) struct OhosWindow {
    handle: crate::AnyWindowHandle,
    app: Rc<RefCell<Option<OpenHarmonyApp>>>,
    bounds: RefCell<Bounds<Pixels>>,
    scale: RefCell<f32>,
    keyboard_overlap_device_px: Cell<i32>,
    safe_area_avoidance_enabled: Cell<bool>,
    last_emitted_resize: RefCell<Option<ResizeCallbackState>>,
    input_handler: Rc<RefCell<Option<PlatformInputHandler>>>,
    callbacks: Rc<RefCell<WindowCallbacks>>,
    pending_frame_request: Cell<Option<bool>>,
    renderer: RefCell<Option<WgpuRenderer>>,
    gpu_context: Rc<RefCell<Option<Arc<WgpuContext>>>>,
    foreground_executor: ForegroundExecutor,
    keyboard_visible: Rc<Cell<bool>>,
    pending_touch_scroll: RefCell<Option<PendingTouchScroll>>,
    last_dispatched_touch_position: RefCell<Option<Point<Pixels>>>,
    touch_state: Cell<TouchState>,
    touch_down_timestamp: Cell<Option<Duration>>,
    last_touch_timestamp: Cell<Option<Duration>>,
    touch_hit_boundary: Cell<bool>,
    touch_velocity_tracker: RefCell<TouchVelocityTracker>,
    scroll_animation: RefCell<Option<ScrollAnimation>>,
    scroll_frame_rate_boosted: Cell<bool>,
    pending_touch_click_feedback_cancel: Cell<Option<Modifiers>>,
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
struct ResizeCallbackState {
    content_size: Size<Pixels>,
    scale: f32,
}

#[derive(Clone, Copy)]
struct PendingTouchScroll {
    position: Point<Pixels>,
    modifiers: Modifiers,
    phase: TouchPhase,
}

#[derive(Clone, Copy, Default)]
enum TouchState {
    #[default]
    Idle,
    Pending(TouchPendingState),
    Scrolling(TouchScrollState),
}

#[derive(Clone, Copy)]
struct TouchPendingState {
    start_position: Point<Pixels>,
    last_position: Point<Pixels>,
    cancel_click: bool,
    mouse_down_sent: bool,
}

#[derive(Clone, Copy)]
struct TouchScrollState {
    last_position: Point<Pixels>,
    locked_axis: Axis,
}

#[derive(Clone, Copy)]
struct ScrollAnimation {
    position: Point<Pixels>,
    modifiers: Modifiers,
    initial_velocity: Point<f32>,
    gamma: f32,
    elapsed: f32,
    last_distance: Point<Pixels>,
    last_frame_timestamp: Option<Duration>,
}

#[derive(Clone, Copy)]
struct TouchSample {
    position: Point<Pixels>,
    timestamp: Duration,
}

#[derive(Default)]
struct TouchVelocityTracker {
    samples: VecDeque<TouchSample>,
}

impl TouchVelocityTracker {
    fn reset(&mut self) {
        self.samples.clear();
    }

    fn push(
        &mut self,
        position: Point<Pixels>,
        timestamp: Option<Duration>,
        max_sample_count: usize,
    ) {
        let Some(timestamp) = timestamp else {
            return;
        };

        if self
            .samples
            .back()
            .is_some_and(|sample| timestamp < sample.timestamp)
        {
            self.samples.clear();
        }

        self.samples.push_back(TouchSample {
            position,
            timestamp,
        });

        while self.samples.len() > max_sample_count {
            self.samples.pop_front();
        }
    }

    fn velocity(&self, locked_axis: Option<Axis>, sample_window: Duration) -> Point<f32> {
        let Some(last_sample) = self.samples.back() else {
            return point(0.0, 0.0);
        };
        let cutoff = last_sample.timestamp.saturating_sub(sample_window);
        let samples = self
            .samples
            .iter()
            .copied()
            .filter(|sample| sample.timestamp >= cutoff)
            .collect::<Vec<_>>();

        let x = if matches!(locked_axis, Some(Axis::Vertical)) {
            0.0
        } else {
            Self::axis_velocity(&samples, Axis::Horizontal).unwrap_or(0.0) as f32
        };
        let y = if matches!(locked_axis, Some(Axis::Horizontal)) {
            0.0
        } else {
            Self::axis_velocity(&samples, Axis::Vertical).unwrap_or(0.0) as f32
        };

        point(x, y)
    }

    fn axis_velocity(samples: &[TouchSample], axis: Axis) -> Option<f64> {
        if samples.len() < 2 {
            return None;
        }

        Self::quadratic_axis_velocity(samples, axis)
            .or_else(|| Self::linear_axis_velocity(samples, axis))
    }

    fn linear_axis_velocity(samples: &[TouchSample], axis: Axis) -> Option<f64> {
        let first = samples.first()?;
        let last = samples.last()?;
        let elapsed = last.timestamp.checked_sub(first.timestamp)?.as_secs_f64();
        if elapsed <= f64::EPSILON {
            return None;
        }

        Some((Self::axis_position(last, axis) - Self::axis_position(first, axis)) / elapsed)
    }

    fn quadratic_axis_velocity(samples: &[TouchSample], axis: Axis) -> Option<f64> {
        if samples.len() < 3 {
            return None;
        }

        let first_timestamp = samples.first()?.timestamp;
        let mut sum_t = 0.0;
        let mut sum_t2 = 0.0;
        let mut sum_t3 = 0.0;
        let mut sum_t4 = 0.0;
        let mut sum_position = 0.0;
        let mut sum_t_position = 0.0;
        let mut sum_t2_position = 0.0;
        let mut positions = Vec::with_capacity(samples.len());

        for sample in samples {
            let t = sample.timestamp.checked_sub(first_timestamp)?.as_secs_f64();
            let t2 = t * t;
            let position = Self::axis_position(sample, axis);

            sum_t += t;
            sum_t2 += t2;
            sum_t3 += t2 * t;
            sum_t4 += t2 * t2;
            sum_position += position;
            sum_t_position += t * position;
            sum_t2_position += t2 * position;
            positions.push(position);
        }

        let last_t = samples
            .last()?
            .timestamp
            .checked_sub(first_timestamp)?
            .as_secs_f64();
        if last_t <= f64::EPSILON {
            return None;
        }

        let [a, b, _c] = Self::solve_3x3([
            [sum_t4, sum_t3, sum_t2, sum_t2_position],
            [sum_t3, sum_t2, sum_t, sum_t_position],
            [sum_t2, sum_t, samples.len() as f64, sum_position],
        ])?;

        let velocity = 2.0 * a * last_t + b;
        if let Some(increasing) = Self::monotonic_direction(&positions)
            && ((increasing && velocity < 0.0) || (!increasing && velocity > 0.0))
        {
            return None;
        }

        Some(velocity)
    }

    fn axis_position(sample: &TouchSample, axis: Axis) -> f64 {
        match axis {
            Axis::Horizontal => sample.position.x.as_f32() as f64,
            Axis::Vertical => sample.position.y.as_f32() as f64,
        }
    }

    fn monotonic_direction(values: &[f64]) -> Option<bool> {
        let mut direction = None;
        for pair in values.windows(2) {
            let delta = pair[1] - pair[0];
            if delta.abs() <= f64::EPSILON {
                continue;
            }

            let increasing = delta > 0.0;
            if let Some(direction) = direction {
                if direction != increasing {
                    return None;
                }
            } else {
                direction = Some(increasing);
            }
        }
        direction
    }

    fn solve_3x3(mut matrix: [[f64; 4]; 3]) -> Option<[f64; 3]> {
        for pivot in 0..3 {
            let mut pivot_row = pivot;
            for row in pivot + 1..3 {
                if matrix[row][pivot].abs() > matrix[pivot_row][pivot].abs() {
                    pivot_row = row;
                }
            }

            let pivot_value = matrix[pivot_row][pivot];
            if pivot_value.abs() <= f64::EPSILON {
                return None;
            }

            if pivot_row != pivot {
                matrix.swap(pivot, pivot_row);
            }

            for col in pivot..4 {
                matrix[pivot][col] /= pivot_value;
            }

            for row in 0..3 {
                if row == pivot {
                    continue;
                }

                let factor = matrix[row][pivot];
                for col in pivot..4 {
                    matrix[row][col] -= factor * matrix[pivot][col];
                }
            }
        }

        Some([matrix[0][3], matrix[1][3], matrix[2][3]])
    }
}

impl OhosWindow {
    const TOUCH_SLOP: f32 = 5.0;
    const TAP_MAX_DURATION: Duration = Duration::from_millis(220);
    const MIN_MOMENTUM_VELOCITY: f32 = 240.0;
    const MAX_MOMENTUM_VELOCITY: f32 = 9_000.0;
    const FLING_VELOCITY_SCALE: f32 = 1.5;
    const FLING_FRICTION: f32 = 0.75;
    const SLOW_FLING_FRICTION: f32 = 1.0;
    const SLOW_FLING_THRESHOLD: f32 = 3_000.0;
    const FRICTION_SCALE: f32 = 4.2;
    const TOUCH_VELOCITY_WINDOW: Duration = Duration::from_millis(100);
    const MAX_TOUCH_SAMPLE_COUNT: usize = 16;
    const DEFAULT_FRAME_INTERVAL_SECONDS: f32 = 1.0 / 120.0;
    const MIN_FRAME_INTERVAL_SECONDS: f32 = 1.0 / 240.0;
    const MAX_FRAME_INTERVAL_SECONDS: f32 = 0.05;
    const MIN_SCROLL_DELTA: Pixels = px(0.1);
    const MAX_MOMENTUM_GAP: Duration = Duration::from_millis(100);

    pub(crate) fn new(
        app: Rc<RefCell<Option<OpenHarmonyApp>>>,
        handle: crate::AnyWindowHandle,
        params: WindowParams,
        gpu_context: Rc<RefCell<Option<Arc<WgpuContext>>>>,
        foreground_executor: ForegroundExecutor,
    ) -> Result<Self> {
        let scale = app
            .borrow()
            .as_ref()
            .map(|a| a.scale() as f32)
            .unwrap_or(1.0);
        let bounds = Bounds::new(point(px(0.0), px(0.0)), params.bounds.size);
        // Don't create renderer immediately - native_window may not be available yet.
        // Renderer will be initialized lazily in draw() or when SurfaceCreate event is received.
        // At that point, native_window from OpenHarmonyApp will be available.

        Ok(Self {
            handle,
            app: app.clone(),
            bounds: RefCell::new(bounds),
            scale: RefCell::new(scale),
            keyboard_overlap_device_px: Cell::new(0),
            safe_area_avoidance_enabled: Cell::new(true),
            last_emitted_resize: RefCell::new(None),
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
            pending_frame_request: Cell::new(None),
            renderer: RefCell::new(None),
            gpu_context,
            foreground_executor,
            keyboard_visible: Rc::new(Cell::new(false)),
            pending_touch_scroll: RefCell::new(None),
            last_dispatched_touch_position: RefCell::new(None),
            touch_state: Cell::new(TouchState::Idle),
            touch_down_timestamp: Cell::new(None),
            last_touch_timestamp: Cell::new(None),
            touch_hit_boundary: Cell::new(false),
            touch_velocity_tracker: RefCell::new(TouchVelocityTracker::default()),
            scroll_animation: RefCell::new(None),
            scroll_frame_rate_boosted: Cell::new(false),
            pending_touch_click_feedback_cancel: Cell::new(None),
        })
    }

    pub(crate) fn handle(&self) -> crate::AnyWindowHandle {
        self.handle
    }

    fn reset_touch_state(&self) {
        *self.pending_touch_scroll.borrow_mut() = None;
        *self.last_dispatched_touch_position.borrow_mut() = None;
        self.touch_state.set(TouchState::Idle);
        self.touch_down_timestamp.set(None);
        self.last_touch_timestamp.set(None);
        self.touch_hit_boundary.set(false);
    }

    fn size_matches(left: Size<Pixels>, right: Size<Pixels>) -> bool {
        const EPSILON: f32 = 0.01;

        (left.width.as_f32() - right.width.as_f32()).abs() <= EPSILON
            && (left.height.as_f32() - right.height.as_f32()).abs() <= EPSILON
    }

    fn resize_state_matches(left: ResizeCallbackState, right: ResizeCallbackState) -> bool {
        const EPSILON: f32 = 0.01;

        Self::size_matches(left.content_size, right.content_size)
            && (left.scale - right.scale).abs() <= EPSILON
    }

    fn set_bounds_size(&self, new_size: Size<Pixels>) -> bool {
        if Self::size_matches(self.bounds.borrow().size, new_size) {
            return false;
        }

        *self.bounds.borrow_mut() = Bounds::new(point(px(0.0), px(0.0)), new_size);
        true
    }

    fn cancel_momentum(&self) {
        *self.scroll_animation.borrow_mut() = None;
        self.set_scroll_frame_rate_boost(false);
    }

    fn reset_touch_velocity(&self) {
        self.touch_velocity_tracker.borrow_mut().reset();
    }

    fn begin_touch_tracking(
        &self,
        position: Point<Pixels>,
        timestamp: Option<Duration>,
        synthesize_mouse_down: bool,
    ) -> bool {
        let canceled_momentum = self.scroll_animation.borrow().is_some();
        self.cancel_momentum();
        let mouse_down_sent = synthesize_mouse_down && !canceled_momentum;
        *self.pending_touch_scroll.borrow_mut() = None;
        *self.last_dispatched_touch_position.borrow_mut() = Some(position);
        self.touch_state.set(TouchState::Pending(TouchPendingState {
            start_position: position,
            last_position: position,
            cancel_click: canceled_momentum,
            mouse_down_sent,
        }));
        self.touch_down_timestamp.set(timestamp);
        self.last_touch_timestamp.set(timestamp);
        self.touch_hit_boundary.set(false);
        self.reset_touch_velocity();
        self.record_touch_position(position, timestamp);
        mouse_down_sent
    }

    fn movement_exceeds_touch_slop(distance_squared: f32) -> bool {
        distance_squared > Self::TOUCH_SLOP * Self::TOUCH_SLOP
    }

    fn velocity_magnitude(velocity: Point<f32>) -> f32 {
        velocity.x.hypot(velocity.y)
    }

    fn clamp_velocity(velocity: Point<f32>) -> Point<f32> {
        point(
            velocity
                .x
                .clamp(-Self::MAX_MOMENTUM_VELOCITY, Self::MAX_MOMENTUM_VELOCITY),
            velocity
                .y
                .clamp(-Self::MAX_MOMENTUM_VELOCITY, Self::MAX_MOMENTUM_VELOCITY),
        )
    }

    fn touch_timestamp(timestamp: i64) -> Option<Duration> {
        u64::try_from(timestamp).ok().map(Duration::from_nanos)
    }

    fn touch_position(&self, touch_event: &TouchEventData) -> Point<Pixels> {
        let scale = *self.scale.borrow();
        point(px(touch_event.x / scale), px(touch_event.y / scale))
    }

    fn touch_gap_exceeded(&self, now: Option<Duration>) -> bool {
        let (Some(previous_sample_time), Some(now)) = (self.last_touch_timestamp.get(), now) else {
            return false;
        };

        now.saturating_sub(previous_sample_time) > Self::MAX_MOMENTUM_GAP
    }

    fn tap_duration_exceeded(&self, now: Option<Duration>) -> bool {
        let (Some(touch_down_time), Some(now)) = (self.touch_down_timestamp.get(), now) else {
            return true;
        };

        now.saturating_sub(touch_down_time) > Self::TAP_MAX_DURATION
    }

    fn record_touch_position(&self, position: Point<Pixels>, timestamp: Option<Duration>) {
        if let Some(timestamp) = timestamp {
            self.last_touch_timestamp.set(Some(timestamp));
        }
        self.touch_velocity_tracker.borrow_mut().push(
            position,
            timestamp,
            Self::MAX_TOUCH_SAMPLE_COUNT,
        );
    }

    fn tracked_touch_velocity(&self) -> Point<f32> {
        self.touch_velocity_tracker
            .borrow()
            .velocity(self.touch_locked_axis(), Self::TOUCH_VELOCITY_WINDOW)
    }

    fn touch_locked_axis(&self) -> Option<Axis> {
        match self.touch_state.get() {
            TouchState::Scrolling(state) => Some(state.locked_axis),
            TouchState::Idle | TouchState::Pending(..) => None,
        }
    }

    fn touch_is_active(&self) -> bool {
        !matches!(self.touch_state.get(), TouchState::Idle)
    }

    fn touch_is_scrolling(&self) -> bool {
        matches!(self.touch_state.get(), TouchState::Scrolling(..))
    }

    fn touch_axis(delta_from_start: Point<Pixels>) -> Axis {
        if delta_from_start.x.abs() > delta_from_start.y.abs() {
            Axis::Horizontal
        } else {
            Axis::Vertical
        }
    }

    fn filter_touch_delta(&self, delta: Point<Pixels>) -> Point<Pixels> {
        Self::filter_touch_delta_with_axis(delta, self.touch_locked_axis())
    }

    fn filter_touch_delta_with_axis(
        delta: Point<Pixels>,
        locked_axis: Option<Axis>,
    ) -> Point<Pixels> {
        match locked_axis {
            Some(Axis::Vertical) => point(px(0.0), delta.y),
            Some(Axis::Horizontal) => point(delta.x, px(0.0)),
            None => delta,
        }
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

    fn queue_pending_touch_scroll(
        &self,
        position: Point<Pixels>,
        modifiers: Modifiers,
        phase: TouchPhase,
    ) {
        let mut pending = self.pending_touch_scroll.borrow_mut();
        if let Some(existing) = pending.as_mut() {
            existing.position = position;
            existing.modifiers = modifiers;
            if matches!(existing.phase, TouchPhase::Moved) && matches!(phase, TouchPhase::Started) {
                existing.phase = TouchPhase::Started;
            }
        } else {
            *pending = Some(PendingTouchScroll {
                position,
                modifiers,
                phase,
            });
        }
    }

    fn cancel_touch_click_feedback(&self, modifiers: Modifiers) {
        // The immediate release cancels pending clicks. The deferred release runs after GPUI has
        // repainted the active-state mouse-up listener that is created in response to mouse-down.
        self.dispatch_touch_click_feedback_cancel(modifiers);
        self.pending_touch_click_feedback_cancel
            .set(Some(modifiers));
    }

    fn dispatch_touch_click_feedback_cancel(&self, modifiers: Modifiers) {
        self.dispatch_input(PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position: point(px(-1.0), px(-1.0)),
            modifiers,
            click_count: 1,
        }));
    }

    fn flush_pending_touch_click_feedback_cancel(&self) {
        if let Some(modifiers) = self.pending_touch_click_feedback_cancel.take() {
            self.dispatch_touch_click_feedback_cancel(modifiers);
        }
    }

    fn clear_touch_hover_feedback(&self, modifiers: Modifiers) {
        self.dispatch_input(PlatformInput::MouseMove(MouseMoveEvent {
            position: point(px(-1.0), px(-1.0)),
            pressed_button: None,
            modifiers,
        }));
    }

    fn dispatch_touch_scroll_wheel(
        &self,
        scroll_wheel_event: ScrollWheelEvent,
    ) -> crate::DispatchEventResult {
        let modifiers = scroll_wheel_event.modifiers;
        let result = Self::dispatch_input_with_callbacks(
            &self.callbacks,
            PlatformInput::ScrollWheel(scroll_wheel_event),
        );
        self.clear_touch_hover_feedback(modifiers);
        result
    }

    fn flush_pending_scroll(&self) {
        let pending = self.pending_touch_scroll.borrow_mut().take();
        let Some(pending) = pending else {
            return;
        };

        let Some(last_position) = *self.last_dispatched_touch_position.borrow() else {
            *self.last_dispatched_touch_position.borrow_mut() = Some(pending.position);
            return;
        };

        let delta = self.filter_touch_delta(point(
            pending.position.x - last_position.x,
            pending.position.y - last_position.y,
        ));
        *self.last_dispatched_touch_position.borrow_mut() = Some(pending.position);

        if delta.x.as_f32() == 0.0 && delta.y.as_f32() == 0.0 {
            return;
        }

        let result = self.dispatch_touch_scroll_wheel(ScrollWheelEvent {
            position: pending.position,
            delta: ScrollDelta::Pixels(delta),
            modifiers: pending.modifiers,
            touch_phase: pending.phase,
        });

        self.touch_hit_boundary.set(result.propagate);
    }

    fn begin_scroll_animation(
        &self,
        position: Point<Pixels>,
        modifiers: Modifiers,
        velocity: Point<f32>,
        friction: f32,
    ) {
        self.touch_hit_boundary.set(false);
        self.set_scroll_frame_rate_boost(true);
        *self.scroll_animation.borrow_mut() = Some(ScrollAnimation {
            position,
            modifiers,
            initial_velocity: velocity,
            gamma: friction * Self::FRICTION_SCALE,
            elapsed: 0.0,
            last_distance: point(px(0.0), px(0.0)),
            last_frame_timestamp: None,
        });
    }

    fn set_scroll_frame_rate_boost(&self, boosted: bool) {
        if self.scroll_frame_rate_boosted.get() == boosted {
            return;
        }

        if let Some(app) = self.app.borrow().as_ref() {
            if boosted {
                app.set_frame_rate(60, 120, 120);
            } else {
                app.set_frame_rate(30, 120, 60);
            }
        }
        self.scroll_frame_rate_boosted.set(boosted);
    }

    fn scroll_frame_timestamp(event_timestamp: i64) -> Option<Duration> {
        u64::try_from(event_timestamp)
            .ok()
            .map(Duration::from_nanos)
    }

    fn animation_frame_interval(
        animation: &mut ScrollAnimation,
        frame_timestamp: Option<Duration>,
    ) -> f32 {
        let Some(frame_timestamp) = frame_timestamp else {
            return Self::DEFAULT_FRAME_INTERVAL_SECONDS;
        };

        let elapsed = animation
            .last_frame_timestamp
            .and_then(|last_frame_timestamp| frame_timestamp.checked_sub(last_frame_timestamp))
            .map(|elapsed| elapsed.as_secs_f32())
            .filter(|elapsed| *elapsed > 0.0)
            .unwrap_or(Self::DEFAULT_FRAME_INTERVAL_SECONDS);

        animation.last_frame_timestamp = Some(frame_timestamp);
        elapsed.clamp(
            Self::MIN_FRAME_INTERVAL_SECONDS,
            Self::MAX_FRAME_INTERVAL_SECONDS,
        )
    }

    fn scroll_animation_distance(velocity: Point<f32>, gamma: f32, elapsed: f32) -> Point<Pixels> {
        if gamma <= f32::EPSILON {
            return point(px(0.0), px(0.0));
        }

        let coefficient = (1.0 - (-gamma * elapsed).exp()) / gamma;
        point(px(velocity.x * coefficient), px(velocity.y * coefficient))
    }

    fn scroll_animation_velocity(velocity: Point<f32>, gamma: f32, elapsed: f32) -> Point<f32> {
        let decay = (-gamma * elapsed).exp();
        point(velocity.x * decay, velocity.y * decay)
    }

    fn advance_scroll_animation(&self, frame_timestamp: Option<Duration>) {
        let Some(mut animation) = self.scroll_animation.borrow_mut().take() else {
            return;
        };

        let frame_interval = Self::animation_frame_interval(&mut animation, frame_timestamp);
        animation.elapsed += frame_interval;

        let current_distance = Self::scroll_animation_distance(
            animation.initial_velocity,
            animation.gamma,
            animation.elapsed,
        );
        let delta = point(
            current_distance.x - animation.last_distance.x,
            current_distance.y - animation.last_distance.y,
        );
        animation.last_distance = current_distance;

        let current_velocity = Self::scroll_animation_velocity(
            animation.initial_velocity,
            animation.gamma,
            animation.elapsed,
        );
        if Self::velocity_magnitude(current_velocity) < Self::MIN_MOMENTUM_VELOCITY
            || (delta.x.abs() < Self::MIN_SCROLL_DELTA && delta.y.abs() < Self::MIN_SCROLL_DELTA)
        {
            self.dispatch_scroll_end(animation.position, animation.modifiers);
            return;
        }

        let result = self.dispatch_touch_scroll_wheel(ScrollWheelEvent {
            position: animation.position,
            delta: ScrollDelta::Pixels(delta),
            modifiers: animation.modifiers,
            touch_phase: TouchPhase::Moved,
        });

        if result.propagate {
            self.dispatch_scroll_end(animation.position, animation.modifiers);
            return;
        }

        *self.scroll_animation.borrow_mut() = Some(animation);
    }

    fn dispatch_scroll_end(&self, position: Point<Pixels>, modifiers: Modifiers) {
        self.flush_pending_scroll();
        self.touch_hit_boundary.set(false);
        *self.scroll_animation.borrow_mut() = None;
        self.set_scroll_frame_rate_boost(false);
        self.dispatch_touch_scroll_wheel(ScrollWheelEvent {
            position,
            delta: ScrollDelta::Pixels(point(px(0.0), px(0.0))),
            modifiers,
            touch_phase: TouchPhase::Ended,
        });
    }

    fn start_momentum_scroll(&self, position: Point<Pixels>, modifiers: Modifiers) {
        let touch_velocity = self.tracked_touch_velocity();
        let friction = if Self::velocity_magnitude(touch_velocity) < Self::SLOW_FLING_THRESHOLD {
            Self::SLOW_FLING_FRICTION
        } else {
            Self::FLING_FRICTION
        };
        let initial_velocity = Self::clamp_velocity(point(
            touch_velocity.x * Self::FLING_VELOCITY_SCALE,
            touch_velocity.y * Self::FLING_VELOCITY_SCALE,
        ));
        if Self::velocity_magnitude(initial_velocity) < Self::MIN_MOMENTUM_VELOCITY {
            self.dispatch_scroll_end(position, modifiers);
            return;
        }
        self.begin_scroll_animation(position, modifiers, initial_velocity, friction);
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
        let resize_state = ResizeCallbackState {
            content_size,
            scale,
        };

        let mut callback = self.callbacks.borrow_mut().resize.take();
        if callback.is_none() {
            self.callbacks.borrow_mut().resize = callback;
            return;
        }

        {
            let mut last_emitted_resize = self.last_emitted_resize.borrow_mut();
            if last_emitted_resize
                .as_ref()
                .copied()
                .is_some_and(|last_resize| Self::resize_state_matches(last_resize, resize_state))
            {
                self.callbacks.borrow_mut().resize = callback;
                return;
            }
            *last_emitted_resize = Some(resize_state);
        }

        if let Some(ref mut cb) = callback {
            cb(content_size, scale);
        }
        self.callbacks.borrow_mut().resize = callback;
    }

    fn request_frame(&self, force_render: bool) {
        let mut callback = self.callbacks.borrow_mut().request_frame.take();
        if let Some(ref mut callback) = callback {
            self.pending_frame_request.set(None);
            callback(RequestFrameOptions {
                require_presentation: force_render,
                force_render,
            });
        } else {
            let force_render = self
                .pending_frame_request
                .take()
                .is_some_and(|pending_force_render| pending_force_render)
                || force_render;
            self.pending_frame_request.set(Some(force_render));
            warn!("OhosWindow: request_frame called before callback was set");
        }
        self.callbacks.borrow_mut().request_frame = callback;
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

        // Update window bounds to match actual content_rect (convert device px -> logical px)
        if content_rect.width > 0 && content_rect.height > 0 {
            let logical_size = size(
                px(device_width as f32 / scale),
                px(device_height as f32 / scale),
            );
            *self.bounds.borrow_mut() = Bounds::new(point(px(0.0), px(0.0)), logical_size);
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
            Ok(_) => {}
            Err(e) => {
                warn!("failed to get OHOS window handle: {:?}", e);
                return Err(anyhow::anyhow!("Window handle not available: {:?}", e));
            }
        }

        let gpu_context = if let Some(gpu_context) = self.gpu_context.borrow().clone() {
            gpu_context
        } else {
            let gpu_context = Arc::new(WgpuContext::new().map_err(|error| {
                warn!("failed to create OHOS GPU context: {error}");
                anyhow::anyhow!("Failed to create GPU context: {error}")
            })?);
            *self.gpu_context.borrow_mut() = Some(gpu_context.clone());
            gpu_context
        };

        // Create renderer using the window's HasWindowHandle and HasDisplayHandle implementation
        // which will get the raw_window_handle from native_window
        let renderer = WgpuRenderer::new(&gpu_context, self, config)
            .map_err(|e| {
                warn!("failed to initialize OHOS renderer: {}", e);
                anyhow::anyhow!("Failed to create Wgpu renderer: {}. Make sure native_window is available from OpenHarmonyApp.", e)
            })?;

        *renderer_guard = Some(renderer);
        Ok(())
    }

    pub(crate) fn handle_event(&self, event: &Event) {
        match event {
            Event::SurfaceCreate => {
                // Initialize renderer when SurfaceCreate event is received
                // Note: on_finish_launching is handled at the platform level (OhosPlatform::handle_ohos_event)
                // before windows are created.
                match self.initialize_renderer() {
                    Ok(()) => {}
                    Err(e) => {
                        warn!(
                            "SurfaceCreate failed to initialize OHOS renderer: {}. Make sure native_window is available from OpenHarmonyApp.",
                            e
                        );
                    }
                }
                if self.refresh_keyboard_overlap_device_px() {
                    self.emit_resize_callback();
                }
                self.request_frame(true);
            }
            Event::WindowResize(ohos_size) => {
                let scale = *self.scale.borrow();
                let width = ohos_size.width as f32;
                let height = ohos_size.height as f32;
                let new_size = size(px(width / scale), px(height / scale));
                let bounds_changed = self.set_bounds_size(new_size);
                let keyboard_overlap_changed = self.refresh_keyboard_overlap_device_px();

                // Update renderer's drawable size
                if bounds_changed && let Some(ref mut renderer) = *self.renderer.borrow_mut() {
                    let device_size = Size {
                        width: DevicePixels(width as i32),
                        height: DevicePixels(height as i32),
                    };
                    renderer.update_drawable_size(device_size);
                }
                if bounds_changed || keyboard_overlap_changed {
                    self.emit_resize_callback();
                    self.request_frame(true);
                }
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
                self.flush_pending_scroll();
                self.advance_scroll_animation(
                    Self::scroll_frame_timestamp(info.target_time_stamp)
                        .or_else(|| Self::scroll_frame_timestamp(info.time_stamp)),
                );
                self.request_frame(false);
                self.flush_pending_touch_click_feedback_cancel();
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
                self.cancel_momentum();
                self.reset_touch_velocity();
                self.reset_touch_state();
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
                let scale_changed = (*self.scale.borrow() - new_scale).abs() > f32::EPSILON;
                *self.scale.borrow_mut() = new_scale;
                let keyboard_overlap_changed = self.refresh_keyboard_overlap_device_px();
                if scale_changed || keyboard_overlap_changed {
                    self.emit_resize_callback();
                    self.request_frame(true);
                }
            }
            Event::WindowDestroy => {
                self.cancel_momentum();
                self.reset_touch_velocity();
                self.reset_touch_state();
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
            InputEvent::ImeEvent(ime_event) => {
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
            InputEvent::TouchEvent(touch_event) => {
                let position = self.touch_position(touch_event);
                let modifiers = Modifiers::default();
                let event_timestamp = Self::touch_timestamp(touch_event.timestamp);

                match touch_event.event_type {
                    TouchEvent::Down => {
                        if self.begin_touch_tracking(position, event_timestamp, true) {
                            self.dispatch_input(PlatformInput::MouseDown(MouseDownEvent {
                                button: MouseButton::Left,
                                position,
                                modifiers,
                                click_count: 1,
                                first_mouse: false,
                            }));
                        }
                    }
                    TouchEvent::Up => {
                        match self.touch_state.get() {
                            TouchState::Scrolling(scroll_state) => {
                                let velocity_is_stale = self.touch_gap_exceeded(event_timestamp);
                                self.record_touch_position(position, event_timestamp);
                                let delta = Self::filter_touch_delta_with_axis(
                                    point(
                                        position.x - scroll_state.last_position.x,
                                        position.y - scroll_state.last_position.y,
                                    ),
                                    Some(scroll_state.locked_axis),
                                );
                                if delta.x.as_f32() != 0.0 || delta.y.as_f32() != 0.0 {
                                    self.queue_pending_touch_scroll(
                                        position,
                                        modifiers,
                                        TouchPhase::Moved,
                                    );
                                }

                                self.flush_pending_scroll();
                                if self.touch_hit_boundary.get() {
                                    self.reset_touch_velocity();
                                    self.dispatch_scroll_end(position, modifiers);
                                } else if velocity_is_stale {
                                    self.reset_touch_velocity();
                                    self.dispatch_scroll_end(position, modifiers);
                                } else {
                                    self.start_momentum_scroll(position, modifiers);
                                }
                            }
                            TouchState::Pending(pending_state)
                                if !pending_state.cancel_click
                                    && !self.tap_duration_exceeded(event_timestamp) =>
                            {
                                if pending_state.mouse_down_sent {
                                    self.dispatch_input(PlatformInput::MouseUp(MouseUpEvent {
                                        button: MouseButton::Left,
                                        position,
                                        modifiers,
                                        click_count: 1,
                                    }));
                                }
                            }
                            TouchState::Pending(pending_state) if pending_state.mouse_down_sent => {
                                self.cancel_touch_click_feedback(modifiers);
                            }
                            TouchState::Idle | TouchState::Pending(..) => {}
                        }

                        self.reset_touch_state();
                    }
                    TouchEvent::Move => {
                        let pressed_point_count = touch_event
                            .touch_points
                            .iter()
                            .filter(|point| point.is_pressed)
                            .count();

                        if !self.touch_is_active() {
                            self.begin_touch_tracking(position, event_timestamp, false);
                        }

                        match self.touch_state.get() {
                            TouchState::Idle => {}
                            TouchState::Pending(mut pending_state) => {
                                if pressed_point_count > 1 {
                                    if pending_state.mouse_down_sent {
                                        self.cancel_touch_click_feedback(modifiers);
                                        pending_state.mouse_down_sent = false;
                                    }
                                    pending_state.cancel_click = true;
                                }

                                let from_start = point(
                                    position.x - pending_state.start_position.x,
                                    position.y - pending_state.start_position.y,
                                );
                                let from_start_sq = from_start.x.as_f32() * from_start.x.as_f32()
                                    + from_start.y.as_f32() * from_start.y.as_f32();
                                self.record_touch_position(position, event_timestamp);

                                if Self::movement_exceeds_touch_slop(from_start_sq) {
                                    let locked_axis = Self::touch_axis(from_start);
                                    let delta = Self::filter_touch_delta_with_axis(
                                        point(
                                            position.x - pending_state.last_position.x,
                                            position.y - pending_state.last_position.y,
                                        ),
                                        Some(locked_axis),
                                    );

                                    if pending_state.mouse_down_sent {
                                        self.cancel_touch_click_feedback(modifiers);
                                    }
                                    self.set_scroll_frame_rate_boost(true);
                                    *self.last_dispatched_touch_position.borrow_mut() =
                                        Some(pending_state.last_position);
                                    self.touch_state
                                        .set(TouchState::Scrolling(TouchScrollState {
                                            last_position: position,
                                            locked_axis,
                                        }));

                                    if delta.x.as_f32() != 0.0 || delta.y.as_f32() != 0.0 {
                                        self.queue_pending_touch_scroll(
                                            position,
                                            modifiers,
                                            TouchPhase::Started,
                                        );
                                    }
                                } else {
                                    pending_state.last_position = position;
                                    self.touch_state.set(TouchState::Pending(pending_state));
                                }
                            }
                            TouchState::Scrolling(mut scroll_state) => {
                                let raw_delta = point(
                                    position.x - scroll_state.last_position.x,
                                    position.y - scroll_state.last_position.y,
                                );
                                let delta = Self::filter_touch_delta_with_axis(
                                    raw_delta,
                                    Some(scroll_state.locked_axis),
                                );
                                self.record_touch_position(position, event_timestamp);
                                if delta.x.as_f32() != 0.0 || delta.y.as_f32() != 0.0 {
                                    self.queue_pending_touch_scroll(
                                        position,
                                        modifiers,
                                        TouchPhase::Moved,
                                    );
                                }
                                scroll_state.last_position = position;
                                self.touch_state.set(TouchState::Scrolling(scroll_state));
                            }
                        }
                    }
                    TouchEvent::Cancel | TouchEvent::Unknown => {
                        if self.touch_is_active() {
                            self.cancel_touch_click_feedback(modifiers);
                        }
                        if self.touch_is_scrolling() {
                            self.dispatch_scroll_end(position, modifiers);
                        }
                        self.cancel_momentum();
                        self.reset_touch_velocity();
                        self.reset_touch_state();
                    }
                }
            }
            _ => {}
        }
    }

    fn dispatch_input(&self, input: PlatformInput) {
        Self::dispatch_input_with_callbacks(&self.callbacks, input);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_sample(tracker: &mut TouchVelocityTracker, x: f32, y: f32, timestamp_ms: u64) {
        tracker.push(
            point(px(x), px(y)),
            Some(Duration::from_millis(timestamp_ms)),
            OhosWindow::MAX_TOUCH_SAMPLE_COUNT,
        );
    }

    #[test]
    fn velocity_tracker_uses_recent_window() {
        let mut tracker = TouchVelocityTracker::default();
        push_sample(&mut tracker, 0.0, 0.0, 0);
        push_sample(&mut tracker, 40.0, 0.0, 40);
        push_sample(&mut tracker, 120.0, 0.0, 120);
        push_sample(&mut tracker, 200.0, 0.0, 200);

        let velocity = tracker.velocity(None, Duration::from_millis(100));

        assert!((velocity.x - 1_000.0).abs() < 0.01);
        assert_eq!(velocity.y, 0.0);
    }

    #[test]
    fn velocity_tracker_respects_axis_lock() {
        let mut tracker = TouchVelocityTracker::default();
        push_sample(&mut tracker, 0.0, 0.0, 0);
        push_sample(&mut tracker, 40.0, 80.0, 40);
        push_sample(&mut tracker, 80.0, 160.0, 80);

        let velocity = tracker.velocity(Some(Axis::Vertical), Duration::from_millis(100));

        assert_eq!(velocity.x, 0.0);
        assert!((velocity.y - 2_000.0).abs() < 0.01);
    }

    #[test]
    fn friction_distance_approaches_arkui_final_position() {
        let gamma = OhosWindow::FLING_FRICTION * OhosWindow::FRICTION_SCALE;
        let velocity = point(1_000.0, 0.0);

        let distance = OhosWindow::scroll_animation_distance(velocity, gamma, 20.0);

        assert!((distance.x.as_f32() - 1_000.0 / gamma).abs() < 0.01);
        assert_eq!(distance.y, px(0.0));
    }

    #[test]
    fn touch_slop_is_shared_by_tap_and_scroll_arbitration() {
        let slop_squared = OhosWindow::TOUCH_SLOP * OhosWindow::TOUCH_SLOP;

        assert!(!OhosWindow::movement_exceeds_touch_slop(slop_squared));
        assert!(OhosWindow::movement_exceeds_touch_slop(slop_squared + 0.01));
    }

    #[test]
    fn touch_axis_prefers_dominant_direction() {
        assert!(matches!(
            OhosWindow::touch_axis(point(px(12.0), px(4.0))),
            Axis::Horizontal
        ));
        assert!(matches!(
            OhosWindow::touch_axis(point(px(4.0), px(12.0))),
            Axis::Vertical
        ));
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

    fn completed_frame(&self) {
        if self.input_handler.borrow().is_none() {
            self.with_window(|window| window.hide_keyboard_if_needed());
        }
        self.with_window(|window| window.completed_frame())
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
        *self.bounds.borrow_mut() = Bounds::new(point(px(0.0), px(0.0)), size);
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
        if let Some(force_render) = self.pending_frame_request.take() {
            self.request_frame(force_render);
        }
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
                warn!("failed to initialize OHOS renderer in draw(): {}", e);
                return;
            }
        }

        // Use WGPU renderer to render the scene.
        if let Some(ref mut renderer) = *self.renderer.borrow_mut() {
            renderer.draw(scene);
        } else {
            warn!("draw called but OHOS renderer is not available");
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
