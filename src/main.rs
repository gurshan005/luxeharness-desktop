mod agent_ui;
mod app;
mod backdrop;
mod harness;
mod icons;
mod session;
mod theme;
mod workspace;
mod workspace_ui;

use app::App;
use iced::{Font, Size, window};

fn main() -> iced::Result {
    let mut settings = window::Settings {
        size: Size::new(1200.0, 800.0),
        decorations: true,
        min_size: Some(Size::new(640.0, 480.0)),
        ..Default::default()
    };
    #[cfg(target_os = "macos")]
    {
        settings.platform_specific.title_hidden = true;
        settings.platform_specific.titlebar_transparent = true;
        settings.platform_specific.fullsize_content_view = true;
    }
    iced::application(App::new, App::update, App::view)
        .title("LuxeHarness")
        .window(settings)
        .centered()
        .antialiasing(true)
        .default_font(Font::with_name("Geist"))
        .font(include_bytes!("../assets/fonts/Geist.ttf").as_slice())
        .theme(App::theme)
        .subscription(App::subscription)
        .run()
}
