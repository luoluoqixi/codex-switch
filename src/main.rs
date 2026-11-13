use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::*;

struct HelloWorld;

impl Render for HelloWorld {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .child("Hello, World!")
            .child(
                Button::new("hello")
                    .primary()
                    .label("Click me")
                    .on_click(|_, _, _| println!("Clicked!")),
            )
    }
}

fn main() {
    application().with_assets(assets::Assets).run(|cx| {
        init(cx);
        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(800.0), px(500.0)),
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some(env!("CARGO_PKG_NAME").into()),
                ..Default::default()
            }),
            ..WindowOptions::default()
        };
        open_window(window_options, cx, |_, cx| cx.new(|_| HelloWorld))
            .expect("Failed to open window");
    });
}
