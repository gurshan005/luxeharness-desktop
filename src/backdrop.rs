use iced::widget::canvas::{self, gradient, path};
use iced::{Color, Point, Rectangle, Renderer, Size, Theme, color, mouse};

#[derive(Default)]
pub struct Backdrop {
    cache: canvas::Cache,
}

const DITHER: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

// Tiny ASCII details sit within the four-pixel cells, rather than defining their size.
const GLYPHS: [[u8; 5]; 5] = [
    [0, 0, 4, 0, 0],
    [0, 4, 0, 4, 0],
    [4, 4, 31, 4, 4],
    [0, 31, 0, 31, 0],
    [10, 31, 10, 31, 10],
];

impl<Message> canvas::Program<Message> for Backdrop {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        vec![self.cache.draw(renderer, bounds.size(), |frame| {
            let w = frame.width();
            let h = frame.height();
            if w <= 0.0 || h <= 0.0 {
                return;
            }
            frame.fill_rectangle(
                Point::ORIGIN,
                frame.size(),
                gradient::Linear::new(Point::ORIGIN, Point::new(0.0, h))
                    .add_stop(0.0, color!(0xF7F9F2))
                    .add_stop(0.42, color!(0xF1F6E9))
                    .add_stop(0.60, color!(0xE2EBD2))
                    .add_stop(0.80, color!(0xB3CBA2))
                    .add_stop(1.0, color!(0x527858)),
            );

            let cell = 4.0;
            let band = (h * 0.54).clamp(192.0, 432.0);
            let rows = (band / cell).ceil() as u32;
            let columns = (w / cell).ceil() as u32;
            let top = h - rows as f32 * cell;
            let ink = color!(0x244D34);

            // The eased coverage and opacity let fine pixels dissolve into the working surface.
            for row in 0..rows {
                let t = row as f32 / (rows - 1).max(1) as f32;
                let fade = t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
                let y = top + row as f32 * cell;
                let mut shadows = path::Builder::new();
                let mut mids = path::Builder::new();
                let mut lights = path::Builder::new();
                let mut glyphs = path::Builder::new();

                for col in 0..columns {
                    let x = col as f32 * cell;
                    let dx = (x - w * 0.5) / (w * 0.38);
                    let dy = (y - h * 0.53) / (h * 0.20);
                    let clearing = (-(dx * dx + dy * dy) * 2.0).exp();
                    let density = (fade * (1.0 - clearing * 0.88) * 1.08).clamp(0.0, 1.0);
                    let threshold =
                        (DITHER[row as usize % 8][col as usize % 8] as f32 + 0.5) / 64.0;
                    if density <= threshold {
                        continue;
                    }

                    let grain = noise(col + row * 977 + 31);
                    let field = noise(col / 4 + (row / 4) * 409 + 73);
                    let tone = grain * 0.42 + field * 0.58;
                    let tiles = if tone < 0.24 {
                        &mut lights
                    } else if tone < 0.49 {
                        &mut mids
                    } else {
                        &mut shadows
                    };
                    tiles.rectangle(Point::new(x, y), Size::new(3.5, 3.5));

                    if (col + row * 3) % 5 == 0 {
                        let glyph = GLYPHS[(grain * GLYPHS.len() as f32) as usize % GLYPHS.len()];
                        for (gy, mask) in glyph.into_iter().enumerate() {
                            for gx in 0..5 {
                                if mask & (1 << (4 - gx)) != 0 {
                                    glyphs.rectangle(
                                        Point::new(
                                            x + 0.5 + gx as f32 * 0.5,
                                            y + 0.5 + gy as f32 * 0.5,
                                        ),
                                        Size::new(0.5, 0.5),
                                    );
                                }
                            }
                        }
                    }
                }
                frame.fill(
                    &shadows.build(),
                    Color {
                        a: fade * 0.46,
                        ..ink
                    },
                );
                frame.fill(
                    &mids.build(),
                    Color {
                        a: fade * 0.32,
                        ..color!(0x7C9B68)
                    },
                );
                frame.fill(
                    &lights.build(),
                    Color {
                        a: fade * 0.24,
                        ..color!(0xC6D9A9)
                    },
                );
                frame.fill(
                    &glyphs.build(),
                    Color {
                        a: fade * 0.34,
                        ..ink
                    },
                );
            }
        })]
    }
}

fn noise(seed: u32) -> f32 {
    let mut value = seed.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    value = ((value >> ((value >> 28) + 4)) ^ value).wrapping_mul(277_803_737);
    ((value >> 22) ^ value) as f32 / u32::MAX as f32
}
