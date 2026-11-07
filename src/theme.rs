use iced::widget::{button, container, scrollable};
use iced::{Border, Color, Element, Shadow, Theme, Vector, color};

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub bg: Color,
    pub surface: Color,
    pub raised: Color,
    pub text: Color,
    pub muted: Color,
    pub faint: Color,
    pub border: Color,
    pub accent: Color,
    pub accent_soft: Color,
    pub on_accent: Color,
}

pub fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            bg: color!(0x171717),
            surface: color!(0x202020),
            raised: color!(0x282828),
            text: color!(0xF0F0F0),
            muted: color!(0xAAAAAA),
            faint: color!(0xA0A0A0),
            border: color!(0x383838),
            accent: color!(0xF0F0F0),
            accent_soft: color!(0x303030),
            on_accent: color!(0x181818),
        }
    } else {
        Palette {
            bg: color!(0xF7F9F2),
            surface: color!(0xFEFFFC),
            raised: color!(0xEFF3E9),
            text: color!(0x243D2E),
            muted: color!(0x5E705F),
            faint: color!(0x6B7C68),
            border: color!(0xDBE4D5),
            accent: color!(0x315E42),
            accent_soft: color!(0xE0EBD8),
            on_accent: color!(0xFAFCF7),
        }
    }
}

pub fn app_theme(dark: bool) -> Theme {
    let p = palette(dark);
    Theme::custom(
        if dark { "Luxe Dark" } else { "Luxe Grid" },
        iced::theme::Palette {
            background: p.bg,
            text: p.text,
            primary: p.accent,
            success: p.accent,
            warning: p.muted,
            danger: p.text,
        },
    )
}

pub fn panel(p: Palette) -> container::Style {
    container::Style {
        background: Some(p.surface.into()),
        text_color: Some(p.text),
        border: Border {
            color: p.border,
            width: 1.0,
            radius: 12.0.into(),
        },
        ..Default::default()
    }
}

pub fn flat(color: Color) -> container::Style {
    container::Style {
        background: Some(color.into()),
        ..Default::default()
    }
}

pub fn subtle(p: Palette, status: button::Status) -> button::Style {
    button::Style {
        background: match status {
            button::Status::Hovered | button::Status::Pressed => Some(p.accent_soft.into()),
            _ => None,
        },
        text_color: if status == button::Status::Disabled {
            p.faint
        } else {
            p.text
        },
        border: Border {
            radius: 6.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub fn action<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
) -> button::Button<'a, Message> {
    button(content).padding([9, 14]).style(primary)
}

pub fn primary(_: &Theme, status: button::Status) -> button::Style {
    let p = palette(false);
    let background = match status {
        button::Status::Hovered => color!(0x254B33),
        button::Status::Pressed => color!(0x1C3B28),
        button::Status::Disabled => p.accent_soft,
        button::Status::Active => p.accent,
    };
    button::Style {
        background: Some(background.into()),
        text_color: if status == button::Status::Disabled {
            p.muted
        } else {
            p.on_accent
        },
        border: Border {
            radius: 8.0.into(),
            ..Default::default()
        },
        shadow: Shadow {
            color: Color {
                a: 0.08,
                ..p.accent
            },
            offset: Vector::new(0.0, 2.0),
            blur_radius: 5.0,
        },
        ..Default::default()
    }
}

pub fn scroll(p: Palette, status: scrollable::Status) -> scrollable::Style {
    let interacting = matches!(
        status,
        scrollable::Status::Hovered {
            is_vertical_scrollbar_hovered: true,
            ..
        } | scrollable::Status::Hovered {
            is_horizontal_scrollbar_hovered: true,
            ..
        } | scrollable::Status::Dragged { .. }
    );
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: if interacting { p.faint } else { p.border }.into(),
            border: Border {
                radius: 3.0.into(),
                ..Default::default()
            },
        },
    };

    scrollable::Style {
        container: container::Style::default(),
        vertical_rail: rail,
        horizontal_rail: rail,
        gap: None,
        auto_scroll: scrollable::AutoScroll {
            background: p.surface.into(),
            border: Border {
                color: p.border,
                width: 1.0,
                radius: 20.0.into(),
            },
            shadow: Default::default(),
            icon: p.muted,
        },
    }
}
