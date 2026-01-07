#[cfg(target_env = "ohos")]
use ohos_hilog_binding::{hilog_debug, hilog_info, hilog_warn};

use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
};

use anyhow::Result;
use futures::channel::oneshot;
use openharmony_ability::{Event, InputEvent, OpenHarmonyApp};

use crate::{
    Action, AnyWindowHandle, App, AppCell, BackgroundExecutor, ClipboardItem, CursorStyle, DisplayId,
    ForegroundExecutor, Keymap, Menu, MenuItem, OwnedMenu, PathPromptOptions, Platform,
    PlatformDisplay, PlatformKeyboardLayout, PlatformKeyboardMapper, PlatformTextSystem,
    PlatformWindow, PriorityQueueReceiver, Result as GpuiResult, RunnableVariant, Task, WindowAppearance, WindowParams,
};
use std::rc::Weak;

use super::{
    dispatcher::OhosDispatcher,
    display::OhosDisplay,
    text_system::OhosTextSystem,
    window::OhosWindow,
};

pub(crate) struct OhosPlatform {
    app: OpenHarmonyApp,
    dispatcher: Arc<OhosDispatcher>,
    background_executor: BackgroundExecutor,
    foreground_executor: ForegroundExecutor,
    text_system: Arc<dyn PlatformTextSystem>,
    primary_display: Rc<OhosDisplay>,
    main_receiver: PriorityQueueReceiver<RunnableVariant>,
}

// Global storage for the gpui_app weak reference
// Note: Using RefCell instead of RwLock because OHOS operations are on the main thread
thread_local! {
    static GPUI_APP: RefCell<Option<Weak<AppCell>>> = RefCell::new(None);
}

pub(crate) fn set_gpui_app_weak(app: Weak<AppCell>) {
    GPUI_APP.with(|cell| *cell.borrow_mut() = Some(app));
}

impl OhosPlatform {
    pub(crate) fn new(app: OpenHarmonyApp) -> Result<Self> {
        let (main_sender, main_receiver) = PriorityQueueReceiver::new();
        let dispatcher = Arc::new(OhosDispatcher::new(main_sender));
        let background_executor = BackgroundExecutor::new(dispatcher.clone());
        let foreground_executor = ForegroundExecutor::new(dispatcher.clone());
        let text_system = Arc::new(OhosTextSystem::new());
        let primary_display = Rc::new(OhosDisplay::new(app.clone()));

        Ok(Self {
            app,
            dispatcher,
            background_executor,
            foreground_executor,
            text_system,
            primary_display,
            main_receiver,
        })
    }
    
    fn run_foreground_tasks(&self) {
        // Process GPUI tasks queued for the main thread
        // Similar to Windows' run_foreground_task, but simpler since OHOS doesn't have message timeouts
        let mut receiver = self.main_receiver.clone();
        while let Ok(Some(runnable)) = receiver.try_pop() {
            OhosDispatcher::execute_runnable(runnable);
        }
    }

    fn handle_ohos_event(&self, event: &Event) {
        hilog_debug!("OhosPlatform: Received event: {:?}", event);
        
        // First, process any GPUI tasks queued for the main thread
        // This ensures tasks are processed in the run_loop, integrating GPUI with OpenHarmony's event loop
        self.run_foreground_tasks();
        
        if let Some(app_weak) = GPUI_APP.with(|cell| cell.borrow().clone()) {
            if let Some(app) = app_weak.upgrade() {
                // Use update() to ensure flush_effects() is called, which processes refresh_windows()
                // This ensures event-driven rendering on OHOS - events drive rendering, not active polling
                app.borrow_mut().update(|app| {
                    // Handle platform-level events and trigger window refresh
                    // On OHOS, rendering is event-driven, so we refresh windows on relevant events
                    match event {
                        Event::WindowRedraw { .. } => {
                            hilog_debug!("OhosPlatform: WindowRedraw event - refreshing windows for event-driven render");
                            app.refresh_windows();
                        }
                        Event::WindowResize(size) => {
                            hilog_debug!("OhosPlatform: WindowResize event - size: {}x{}", size.width, size.height);
                            // Window resize is handled by individual windows through their callbacks
                            // Trigger a refresh to update the window
                            app.refresh_windows();
                        }
                        Event::Input(_input_event) => {
                            hilog_debug!("OhosPlatform: Input event received");
                            // Input events are handled by windows through their callbacks
                            // The callbacks are set up in Window::new
                            // Input events may also require rendering, so refresh windows
                            app.refresh_windows();
                        }
                        Event::GainedFocus => {
                            hilog_debug!("OhosPlatform: GainedFocus event");
                            app.refresh_windows();
                        }
                        Event::LostFocus => {
                            hilog_debug!("OhosPlatform: LostFocus event");
                            app.refresh_windows();
                        }
                        Event::ConfigChanged(..) => {
                            hilog_debug!("OhosPlatform: ConfigChanged event");
                            app.refresh_windows();
                        }
                        Event::WindowDestroy => {
                            hilog_debug!("OhosPlatform: WindowDestroy event");
                        }
                        _ => {
                            hilog_debug!("OhosPlatform: Unhandled event: {:?}", event);
                        }
                    }
                });
            } else {
                hilog_warn!("OhosPlatform: App weak reference could not be upgraded");
            }
        } else {
            hilog_warn!("OhosPlatform: No GPUI app weak reference found");
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
        // Call the launch callback first
        on_finish_launching();
        
        // Leak the platform to prevent it from being dropped
        // This is necessary because run_loop doesn't retain ownership
        // The platform is held by Rc in Application, so leaking it here ensures
        // it lives for the lifetime of the application
        // SAFETY: The platform must live for the lifetime of the application
        // Since Application is leaked in app.rs, the platform will also live
        let platform_leaked: &'static Self = unsafe {
            std::mem::transmute(self)
        };
        
        // Start the OpenHarmony event loop (non-blocking)
        // run_loop registers a callback and returns immediately
        // This integrates the OpenHarmony event loop with GPUI
        let app = self.app.clone();
        
        app.run_loop(move |event| {
            // Handle events in the platform
            // The platform is leaked, so it's safe to use here
            platform_leaked.handle_ohos_event(&event);
        });
        
        // run_loop returns immediately (non-blocking)
        // The function returns here, allowing #[ability] to return
    }

    fn quit(&self) {
        self.app.exit(0);
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
        vec![self.primary_display.clone()]
    }

    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        Some(self.primary_display.clone())
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
        Ok(Box::new(OhosWindow::new(
            self.app.clone(),
            handle,
            options,
        )?))
    }

    fn window_appearance(&self) -> WindowAppearance {
        WindowAppearance::Light
    }

    fn open_url(&self, url: &str) {
        // Not supported on OHOS
        hilog_warn!("open_url not supported on OHOS: {}", url);
    }

    fn on_open_urls(&self, _callback: Box<dyn FnMut(Vec<String>)>) {
        // Not supported on OHOS
    }

    fn register_url_scheme(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Err(anyhow::anyhow!("URL scheme registration not supported on OHOS")))
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
        Err(anyhow::anyhow!("path_for_auxiliary_executable not available on OHOS"))
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
        Task::ready(Err(anyhow::anyhow!("Credential storage not supported on OHOS")))
    }

    fn read_credentials(&self, _url: &str) -> Task<Result<Option<(String, Vec<u8>)>>> {
        Task::ready(Ok(None))
    }

    fn delete_credentials(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Err(anyhow::anyhow!("Credential deletion not supported on OHOS")))
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

