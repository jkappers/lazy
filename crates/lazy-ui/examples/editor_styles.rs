//! Renders the live-markdown page editor offscreen, with the caret on different lines, in both
//! appearances, and writes PNGs. Same headless renderer as the `screens` example.
//!
//!     cargo run -p lazy-ui --example editor_styles -- <output-dir>

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    AppContext as _, Context, Entity, Focusable as _, HeadlessAppContext, IntoElement,
    ParentElement as _, Render, Styled as _, Window, WindowAppearance, div, px, size,
};
use lazy_ui::fonts::FAMILY;
use lazy_ui::text_input::{TextInput, TextInputOptions};
use lazy_ui::theme::Theme;

const PAGE: &str = "# Launch plan\n\
\n\
We ship the **docs** to everyone on *Friday*. The ~old plan~ is gone, see `export.rs` and [the notes](https://example.com/notes).\n\
\n\
## Checklist\n\
\n\
- [x] Write the announcement\n\
- [ ] Record a short demo with a rather long description so that it has to wrap onto a second row\n\
- Update the *help centre*\n\
\n\
1. First\n\
2. Second\n\
\n\
> Nothing goes out until the demo is signed off.\n\
\n\
---\n\
\n\
### Code\n\
\n\
```rust\n\
fn main() { println!(\"hi\"); }\n\
```\n\
\n\
日本語のテキスト and an emoji 🎉 end.";

struct Host {
    input: Entity<TextInput>,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.global::<Theme>().colors;
        div()
            .size_full()
            .bg(colors.surface)
            .text_color(colors.text)
            .font_family(FAMILY)
            .p_6()
            .child(div().w(px(640.)).child(self.input.clone()))
    }
}

fn render(out: &std::path::Path, name: &str, appearance: WindowAppearance, caret_in: &str) {
    let mut cx = HeadlessAppContext::with_platform(
        Arc::new(gpui_wgpu::CosmicTextSystem::new("DM Sans")),
        Arc::new(()),
        gpui_platform::current_headless_renderer,
    );
    cx.update(|app| lazy_ui::init(app, appearance));
    let window = cx
        .open_window(size(px(720.), px(1000.)), move |_, app| {
            let input = app.new(|cx| {
                TextInput::new(
                    TextInputOptions::new("Start writing")
                        .live_markdown(true)
                        .max_lines(4_000)
                        .max_bytes(1_000_000),
                    cx,
                )
            });
            app.new(|_| Host { input })
        })
        .expect("open window");
    let input = window
        .read_with(&cx, |host, _| host.input.clone())
        .expect("host");
    cx.update(|app| {
        input.update(app, |input, cx| {
            input.set_text(PAGE, cx);
            let at = PAGE
                .find(caret_in)
                .map_or(PAGE.len(), |at| at + caret_in.len());
            input.set_selection(at, at, cx);
        });
    });
    cx.update_window(window.into(), |_, window, app| {
        let focus = input.read(app).focus_handle(app);
        window.focus(&focus, app);
    })
    .expect("focus");
    for _ in 0..4 {
        cx.update_window(window.into(), |_, window, app| window.draw(app).clear(app))
            .expect("draw");
        cx.run_until_parked();
    }
    let image = cx.capture_screenshot(window.into()).expect("screenshot");
    let path = out.join(format!("{name}.png"));
    image.save(&path).expect("write png");
    println!(
        "wrote {} ({}x{})",
        path.display(),
        image.width(),
        image.height()
    );
}

fn main() {
    let out: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "editor".into())
        .into();
    std::fs::create_dir_all(&out).expect("create output directory");
    render(
        &out,
        "editor-light-caret-in-text",
        WindowAppearance::Light,
        "to everyone",
    );
    render(
        &out,
        "editor-light-caret-in-heading",
        WindowAppearance::Light,
        "Launch",
    );
    render(
        &out,
        "editor-dark-caret-in-task",
        WindowAppearance::Dark,
        "Record a short",
    );
}
