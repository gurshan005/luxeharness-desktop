use iced::Color;
use iced::widget::{Svg, svg};

#[derive(Debug, Clone, Copy)]
pub enum Icon {
    Plus,
    ArrowUp,
    File,
    Folder,
    Close,
    Panel,
    Compose,
    Search,
    Vault,
    AgentControl,
    Plugins,
}

pub fn icon(kind: Icon, size: f32, color: Color) -> Svg<'static> {
    let drawing = match kind {
        Icon::Plus => r#"<path d="M12 5v14M5 12h14"/>"#,
        Icon::ArrowUp => r#"<path d="M12 19V5m-6 6 6-6 6 6"/>"#,
        Icon::File => {
            r#"<path d="M14 3.5H6.5a1 1 0 0 0-1 1v15a1 1 0 0 0 1 1h11a1 1 0 0 0 1-1V8l-4.5-4.5ZM14 3.5V8h4.5M9 12h6M9 16h6"/>"#
        }
        Icon::Close => r#"<path d="m6 6 12 12M18 6 6 18"/>"#,
        Icon::Folder => {
            r#"<path d="M3 7a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7Z"/>"#
        }
        Icon::Panel => {
            r#"<rect x="3.5" y="4.5" width="17" height="15" rx="2"/><path d="M9 4.5v15"/>"#
        }
        Icon::Compose => {
            r#"<path d="M11 4H5a1 1 0 0 0-1 1v14a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1v-6m-4.5-7.5 3 3M17.5 3.5l3 3L11 16l-4 1 1-4 9.5-9.5Z"/>"#
        }
        Icon::Search => r#"<circle cx="10.5" cy="10.5" r="6.5"/><path d="m16 16 4.5 4.5"/>"#,
        Icon::Vault => {
            r#"<rect x="3.5" y="3.5" width="17" height="17" rx="2"/><circle cx="10.5" cy="12" r="3"/><path d="M10.5 9v6m-3-3h6M17 9v6"/>"#
        }
        Icon::AgentControl => {
            r#"<path d="M4 6h4m4 0h8M4 12h9m4 0h3M4 18h2m4 0h10"/><circle cx="10" cy="6" r="2"/><circle cx="15" cy="12" r="2"/><circle cx="8" cy="18" r="2"/>"#
        }
        Icon::Plugins => r#"<path d="M8 3v5m8-5v5M6 8h12v4a6 6 0 0 1-12 0V8Zm6 10v3"/>"#,
    };

    let source = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">{drawing}</svg>"#
    );

    Svg::new(svg::Handle::from_memory(source.into_bytes()))
        .width(size)
        .height(size)
        .style(move |_, _| svg::Style { color: Some(color) })
}
