// SPDX-License-Identifier: GPL-3.0-or-later
//! Renders the desktop mid-rubber-band to a PNM, for eyeballing the outline.
//!
//! The band only exists between a press and a release, so it cannot be caught with a
//! screenshot without a hand on the mouse. This drives the same `Selection` the desktop does
//! and paints one frame.
//!
//! `cargo run --example band_preview -- ~/Desktop out.pnm`
use std::path::PathBuf;
use wlrix_desktop::image::Images;
use wlrix_desktop::layout::{Grid, Metrics, Point, Rect};
use wlrix_desktop::running::Running;
use wlrix_desktop::select::Selection;
use wlrix_desktop::state::State;
use wlrix_desktop::theme::font::Fonts;
use wlrix_desktop::ui::{canvas::Canvas, paint};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("a desktop directory"));
    let out = args.next().unwrap_or_else(|| "band.pnm".into());
    let (w, h) = (900, 560);

    let entries = wlrix_desktop::entries::read(&dir);
    let metrics = Metrics::default();
    let grid = Grid::new(Rect::new(0, 0, w, h), metrics);
    let mut state = State::default();
    let placed = state.arrange(&grid, &entries);

    // Drag a band from the middle of the desktop up over the icon column.
    let mut selection = Selection::default();
    selection.press(&placed, Point::new(w - 340, h - 120), 0);
    selection.motion(&placed, Point::new(w - 20, 40));

    let mut fonts = Fonts::load().expect("fonts");
    let mut images = Images::default();
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    {
        let mut canvas = Canvas::new(&mut pixels, w, h);
        paint::desktop(
            &mut canvas,
            &mut paint::Frame {
                fonts: &mut fonts,
                images: &mut images,
                placed: &placed,
                selection: &selection,
                running: &Running::default(),
                icon_size: metrics.icon,
            },
        );
    }
    // Composite over the desktop gray so the transparent parts are visible.
    let mut pnm = format!("P6\n{w} {h}\n255\n").into_bytes();
    for px in pixels.chunks_exact(4) {
        let a = px[3] as u32;
        let over = |c: u8| ((c as u32 * 255 + 0x55 * (255 - a)) / 255).min(255) as u8;
        pnm.extend_from_slice(&[over(px[2]), over(px[1]), over(px[0])]);
    }
    std::fs::write(&out, pnm).expect("write");
    println!(
        "wrote {out} ({} icons, {} selected)",
        placed.len(),
        selection.selected().len()
    );
}
