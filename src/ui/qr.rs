//! A QR code drawn as quads: dark modules on white with the standard
//! four-module quiet zone, whatever the theme (scanners want dark on light).

use std::rc::Rc;

use gpui_kit::prelude::*;
use gpui_kit::{Bounds, IntoElement, Pixels, canvas, div, fill, point, px, size};

/// Modules around the code that must stay light for scanners to find it.
const QUIET_ZONE: usize = 4;

/// Which modules are dark, row by row.
#[derive(Debug, PartialEq)]
pub struct QrModules {
    width: usize,
    dark: Vec<bool>,
}

impl QrModules {
    pub fn encode(data: &str) -> Option<Self> {
        let code = qrcode::QrCode::new(data.as_bytes()).ok()?;
        Some(Self {
            width: code.width(),
            dark: code
                .to_colors()
                .into_iter()
                .map(|color| color == qrcode::Color::Dark)
                .collect(),
        })
    }
}

/// `modules` as a square of `side`, quiet zone included.
pub fn qr_code(modules: Rc<QrModules>, side: Pixels) -> impl IntoElement {
    div().size(side).bg(gpui::white()).child(
        canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, (), window, _| {
                let cells = modules.width + 2 * QUIET_ZONE;
                let cell = bounds.size.width.min(bounds.size.height) / cells as f32;
                // Whole pixels keep neighbouring modules from blurring.
                let cell = px(f32::from(cell).floor().max(1.));
                let offset = (bounds.size.width - cell * cells as f32) / 2.;
                let origin = bounds.origin + point(offset, offset);
                for (ix, _) in modules.dark.iter().enumerate().filter(|(_, dark)| **dark) {
                    let (row, col) = (ix / modules.width, ix % modules.width);
                    let at = origin
                        + point(
                            cell * (col + QUIET_ZONE) as f32,
                            cell * (row + QUIET_ZONE) as f32,
                        );
                    window.paint_quad(fill(Bounds::new(at, size(cell, cell)), gpui::black()));
                }
            },
        )
        .size_full(),
    )
}
