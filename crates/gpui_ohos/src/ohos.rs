mod dispatcher;
mod display;
mod keyboard;
mod platform;
mod text_system;
mod wgpu_atlas;
mod wgpu_context;
mod wgpu_renderer;
mod window;

pub fn current_platform(_headless: bool) -> std::rc::Rc<dyn gpui::Platform> {
    std::rc::Rc::new(
        platform::OhosPlatform::new()
            .inspect_err(|err| {
                log::error!("Failed to initialize OHOS platform: {}", err);
            })
            .unwrap_or_else(|_| panic!("Failed to initialize OHOS platform")),
    )
}
