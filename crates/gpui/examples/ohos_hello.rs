//! A simple example demonstrating GPUI on OpenHarmony OS
//!
//! This example shows how to create a basic GPUI application on OHOS platform.
//! It requires the `openharmony-ability` and `openharmony-ability-derive` crates.
//!
//! ## Building
//!
//! To build this example for OHOS:
//! ```bash
//! cargo build --target aarch64-unknown-linux-ohos --example ohos_hello
//! ```
//!
//! ## Usage
//!
//! On OHOS, this example will create a window displaying "Hello from OpenHarmony!".
//! The entry point is the `#[ability]` macro, not `main()`.
//!
//! Note: This example must be built as a library (lib.rs) in a real OHOS project,
//! not as an example. The `#[ability]` macro generates the necessary NAPI bindings
//! for the ArkTS entry point.

use gpui::{
    App, Application, Bounds, Context, SharedString, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, rgb, size,
};

use openharmony_ability::OpenHarmonyApp;

// On non-OHOS platforms, we don't need these imports

struct OhosHello {
    text: SharedString,
}

impl Render for OhosHello {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_4()
            .bg(rgb(0x2c3e50))
            .size_full()
            .justify_center()
            .items_center()
            .child(
                div()
                    .text_2xl()
                    .text_color(rgb(0xecf0f1))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(format!("Hello from OpenHarmony! {}", &self.text)),
            )
            .child(
                div()
                    .text_lg()
                    .text_color(rgb(0xbdc3c7))
                    .child("GPUI is running on OHOS"),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .mt_4()
                    .child(
                        div()
                            .size_12()
                            .bg(rgb(0xe74c3c))
                            .rounded_full()
                            .border_2()
                            .border_color(rgb(0xffffff)),
                    )
                    .child(
                        div()
                            .size_12()
                            .bg(rgb(0x27ae60))
                            .rounded_full()
                            .border_2()
                            .border_color(rgb(0xffffff)),
                    )
                    .child(
                        div()
                            .size_12()
                            .bg(rgb(0x3498db))
                            .rounded_full()
                            .border_2()
                            .border_color(rgb(0xffffff)),
                    ),
            )
    }
}

#[openharmony_ability_derive::ability]
pub fn openharmony_app(app: OpenHarmonyApp) {
    // Initialize and run GPUI application
    // The event loop is automatically integrated by the platform
    Application::new()
        .with_ohos_app(app.clone())
        .run(|cx: &mut App| {
            let default_size = size(px(800.0), px(600.0));
            let bounds = Bounds::centered(None, default_size, cx);

            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| {
                    cx.new(|_| OhosHello {
                        text: "OHOS".into(),
                    })
                },
            )
            .unwrap();

            cx.activate(true);
        });
}
