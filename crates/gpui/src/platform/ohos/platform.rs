use log::{debug, warn};

use std::{
    cell::RefCell,
    path::PathBuf,
    rc::{Rc, Weak},
    sync::Arc,
};

use anyhow::Result;
use futures::channel::oneshot;
use openharmony_ability::{Event, OpenHarmonyApp};

use crate::{
    Action, AnyWindowHandle, BackgroundExecutor, ClipboardItem, CursorStyle, DisplayId,
    ForegroundExecutor, Keymap, Menu, MenuItem, OwnedMenu, PathPromptOptions, Platform,
    PlatformDisplay, PlatformKeyboardLayout, PlatformKeyboardMapper, PlatformTextSystem,
    PlatformWindow, PriorityQueueReceiver, Result as GpuiResult, RunnableVariant, Task,
    WindowAppearance, WindowParams,
};

use super::{
    dispatcher::OhosDispatcher, display::OhosDisplay, text_system::OhosTextSystem,
    window::OhosWindow,
};
use crate::platform::blade::BladeContext;

pub(crate) struct OhosPlatform {
    app: Rc<RefCell<Option<OpenHarmonyApp>>>,
    dispatcher: Arc<OhosDispatcher>,
    background_executor: BackgroundExecutor,
    foreground_executor: ForegroundExecutor,
    text_system: Arc<dyn PlatformTextSystem>,
    primary_display: Rc<RefCell<Option<OhosDisplay>>>,
    main_receiver: PriorityQueueReceiver<RunnableVariant>,
    gpu_context: Arc<BladeContext>,
    windows: Rc<RefCell<Vec<Weak<RefCell<OhosWindow>>>>>,
}

impl OhosPlatform {
    pub(crate) fn new() -> Result<Self> {
        let (main_sender, main_receiver) = PriorityQueueReceiver::new();
        let dispatcher = Arc::new(OhosDispatcher::new(main_sender));
        let background_executor = BackgroundExecutor::new(dispatcher.clone());
        let foreground_executor = ForegroundExecutor::new(dispatcher.clone());
        let text_system = Arc::new(OhosTextSystem::new());

        // Initialize GPU context for Blade renderer, same as Linux Wayland
        // Note: ZED_DEVICE_ID environment variable is optional - if not set, device_id defaults to 0
        let gpu_context = Arc::new(BladeContext::new()
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to create GPU context: {}. \
                    Note: ZED_DEVICE_ID environment variable is optional. \
                    If you need to specify a GPU device, set ZED_DEVICE_ID to a 4-digit hex PCI ID (e.g., '0x1234').",
                    e
                )
            })?);

        Ok(Self {
            app: Rc::new(RefCell::new(None)),
            dispatcher,
            background_executor,
            foreground_executor,
            text_system,
            primary_display: Rc::new(RefCell::new(None)),
            main_receiver,
            gpu_context,
            windows: Rc::new(RefCell::new(Vec::new())),
        })
    }

    pub(crate) fn set_app(&self, app: OpenHarmonyApp) {
        *self.app.borrow_mut() = Some(app.clone());
        // Initialize primary display when app is set
        *self.primary_display.borrow_mut() = Some(OhosDisplay::new(app.clone()));
        self.dispatcher.set_waker(app.create_waker());
    }

    pub(crate) fn set_app_from_platform(&self, app: OpenHarmonyApp) {
        self.set_app(app);
    }

    fn run_foreground_tasks(&self) {
        // Process GPUI tasks queued for the main thread
        // Similar to Windows' run_foreground_task, but simpler since OHOS doesn't have message timeouts
        let mut receiver = self.main_receiver.clone();
        while let Ok(Some(runnable)) = receiver.try_pop() {
            OhosDispatcher::execute_runnable(runnable);
        }
    }

    fn handle_ohos_event(&self, event: &Event, on_finish_launching: Option<Box<dyn FnOnce()>>) {
        // First, process any GPUI tasks queued for the main thread
        // This ensures tasks are processed in the run_loop, integrating GPUI with OpenHarmony's event loop
        self.run_foreground_tasks();

        if matches!(event, Event::UserEvent) {
            self.dispatcher.run_due_timers();
        }

        // Handle on_finish_launching callback first, before routing to windows.
        // This is critical because windows are created INSIDE the on_finish_launching callback,
        // so we cannot depend on windows existing before calling it.
        // This is similar to how macOS handles did_finish_launching.
        // Note: The callback is only passed when event is SurfaceCreate (checked in run() method),
        // so we can safely call it here unconditionally.
        if let Some(callback) = on_finish_launching {
            debug!("OhosPlatform: Calling on_finish_launching on SurfaceCreate");
            callback();
        }

        // Route events to all known OHOS windows without borrowing App.
        // This avoids RefCell borrow conflicts when callbacks trigger app updates.
        let mut live_windows: Vec<Rc<RefCell<OhosWindow>>> = Vec::new();
        {
            let mut windows = self.windows.borrow_mut();
            windows.retain(|weak: &Weak<RefCell<OhosWindow>>| {
                if let Some(window) = weak.upgrade() {
                    live_windows.push(window);
                    true
                } else {
                    false
                }
            });
        }

        if live_windows.is_empty() {
            warn!("OhosPlatform: No active windows to handle event");
        }

        for window in live_windows {
            window.borrow().handle_event(event);
        }
    }
}

impl Clone for OhosPlatform {
    fn clone(&self) -> Self {
        Self {
            app: self.app.clone(),
            dispatcher: self.dispatcher.clone(),
            background_executor: self.background_executor.clone(),
            foreground_executor: self.foreground_executor.clone(),
            text_system: self.text_system.clone(),
            primary_display: self.primary_display.clone(),
            main_receiver: self.main_receiver.clone(),
            gpu_context: self.gpu_context.clone(),
            windows: self.windows.clone(),
        }
    }
}

impl Platform for OhosPlatform {
    fn background_executor(&self) -> BackgroundExecutor {
        self.background_executor.clone()
    }

    fn foreground_executor(&self) -> ForegroundExecutor {
        self.foreground_executor.clone()
    }

    fn text_system(&self) -> Arc<dyn PlatformTextSystem> {
        self.text_system.clone()
    }

    fn run(&self, on_finish_launching: Box<dyn 'static + FnOnce()>) {
        let platform = self.clone();
        let on_finish = Rc::new(RefCell::new(Some(on_finish_launching)));
        if let Some(app) = self.app.borrow().clone() {
            let on_finish_clone = on_finish.clone();
            app.run_loop(move |event: Event| {
                // Only take on_finish_launching when we receive SurfaceCreate event
                let callback = if matches!(event, Event::SurfaceCreate { .. }) {
                    on_finish_clone.borrow_mut().take()
                } else {
                    None
                };
                platform.handle_ohos_event(&event, callback);
            });
        } else {
            warn!("OhosPlatform: App not set");
        }
    }

    fn quit(&self) {
        if let Some(app) = self.app.borrow_mut().clone() {
            app.exit(0);
        }
    }

    fn restart(&self, _binary_path: Option<PathBuf>) {
        // Not supported on OHOS
    }

    fn activate(&self, _ignoring_other_apps: bool) {
        // Not supported on OHOS
    }

    fn hide(&self) {
        // Not supported on OHOS
    }

    fn hide_other_apps(&self) {
        // Not supported on OHOS
    }

    fn unhide_other_apps(&self) {
        // Not supported on OHOS
    }

    fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>> {
        if let Some(display) = self.primary_display.borrow().as_ref() {
            vec![Rc::new(display.clone()) as Rc<dyn PlatformDisplay>]
        } else {
            vec![]
        }
    }

    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        self.primary_display
            .borrow()
            .as_ref()
            .map(|d| Rc::new(d.clone()) as Rc<dyn PlatformDisplay>)
    }

    fn active_window(&self) -> Option<AnyWindowHandle> {
        // OHOS typically has a single window
        None
    }

    fn window_stack(&self) -> Option<Vec<AnyWindowHandle>> {
        None
    }

    fn is_screen_capture_supported(&self) -> bool {
        false
    }

    fn screen_capture_sources(
        &self,
    ) -> oneshot::Receiver<GpuiResult<Vec<Rc<dyn crate::ScreenCaptureSource>>>> {
        let (tx, rx) = oneshot::channel();
        tx.send(Err(anyhow::anyhow!("Screen capture not supported on OHOS")))
            .ok();
        rx
    }

    fn open_window(
        &self,
        handle: AnyWindowHandle,
        options: WindowParams,
    ) -> anyhow::Result<Box<dyn PlatformWindow>> {
        if self.app.borrow().is_some() {
            let window = Rc::new(RefCell::new(OhosWindow::new(
                self.app.clone(),
                handle,
                options,
                self.gpu_context.clone(),
                self.foreground_executor.clone(),
            )?));
            self.windows.borrow_mut().push(Rc::downgrade(&window));
            Ok(Box::new(super::window::OhosWindowHandle::new(window)))
        } else {
            Err(anyhow::anyhow!("OpenHarmonyApp not set"))
        }
    }

    #[cfg(target_env = "ohos")]
    fn set_ohos_app(&self, app: OpenHarmonyApp) {
        self.set_app_from_platform(app);
    }

    fn window_appearance(&self) -> WindowAppearance {
        WindowAppearance::Light
    }

    fn open_url(&self, url: &str) {
        // Not supported on OHOS
        warn!("open_url not supported on OHOS: {}", url);
    }

    fn on_open_urls(&self, _callback: Box<dyn FnMut(Vec<String>)>) {
        // Not supported on OHOS
    }

    fn register_url_scheme(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Err(anyhow::anyhow!(
            "URL scheme registration not supported on OHOS"
        )))
    }

    fn prompt_for_paths(
        &self,
        _options: PathPromptOptions,
    ) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>> {
        let (tx, rx) = oneshot::channel();
        tx.send(Ok(None)).ok();
        rx
    }

    fn prompt_for_new_path(
        &self,
        _directory: &std::path::Path,
        _suggested_name: Option<&str>,
    ) -> oneshot::Receiver<Result<Option<PathBuf>>> {
        let (tx, rx) = oneshot::channel();
        tx.send(Ok(None)).ok();
        rx
    }

    fn can_select_mixed_files_and_dirs(&self) -> bool {
        false
    }

    fn reveal_path(&self, _path: &std::path::Path) {
        // Not supported on OHOS
    }

    fn open_with_system(&self, _path: &std::path::Path) {
        // Not supported on OHOS
    }

    fn on_quit(&self, _callback: Box<dyn FnMut()>) {
        // Handled by OpenHarmonyApp lifecycle
    }

    fn on_reopen(&self, _callback: Box<dyn FnMut()>) {
        // Not supported on OHOS
    }

    fn set_menus(&self, _menus: Vec<Menu>, _keymap: &Keymap) {
        // Not supported on OHOS
    }

    fn get_menus(&self) -> Option<Vec<OwnedMenu>> {
        None
    }

    fn set_dock_menu(&self, _menu: Vec<MenuItem>, _keymap: &Keymap) {
        // Not supported on OHOS
    }

    fn on_app_menu_action(&self, _callback: Box<dyn FnMut(&dyn Action)>) {
        // Not supported on OHOS
    }

    fn on_will_open_app_menu(&self, _callback: Box<dyn FnMut()>) {
        // Not supported on OHOS
    }

    fn on_validate_app_menu_command(&self, _callback: Box<dyn FnMut(&dyn Action) -> bool>) {
        // Not supported on OHOS
    }

    fn compositor_name(&self) -> &'static str {
        "OHOS"
    }

    fn app_path(&self) -> Result<PathBuf> {
        Err(anyhow::anyhow!("app_path not available on OHOS"))
    }

    fn path_for_auxiliary_executable(&self, _name: &str) -> Result<PathBuf> {
        Err(anyhow::anyhow!(
            "path_for_auxiliary_executable not available on OHOS"
        ))
    }

    fn set_cursor_style(&self, _style: CursorStyle) {
        // Cursor style is managed by the system on OHOS
    }

    fn should_auto_hide_scrollbars(&self) -> bool {
        false
    }

    fn read_from_clipboard(&self) -> Option<ClipboardItem> {
        // TODO: Implement clipboard support
        None
    }

    fn write_to_clipboard(&self, _item: ClipboardItem) {
        // TODO: Implement clipboard support
    }

    fn write_credentials(&self, _url: &str, _username: &str, _password: &[u8]) -> Task<Result<()>> {
        Task::ready(Err(anyhow::anyhow!(
            "Credential storage not supported on OHOS"
        )))
    }

    fn read_credentials(&self, _url: &str) -> Task<Result<Option<(String, Vec<u8>)>>> {
        Task::ready(Ok(None))
    }

    fn delete_credentials(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Err(anyhow::anyhow!(
            "Credential deletion not supported on OHOS"
        )))
    }

    fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout> {
        Box::new(super::keyboard::OhosKeyboardLayout)
    }

    fn keyboard_mapper(&self) -> Rc<dyn PlatformKeyboardMapper> {
        Rc::new(super::keyboard::OhosKeyboardMapper)
    }

    fn on_keyboard_layout_change(&self, _callback: Box<dyn FnMut()>) {
        // Not supported on OHOS
    }
}
