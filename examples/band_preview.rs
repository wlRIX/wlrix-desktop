// SPDX-License-Identifier: GPL-3.0-or-later
//! Renders one desktop frame to a PNM, for eyeballing things a screenshot cannot catch.
//!
//! The rubber band and the right-click menu only exist between a press and a release, or
//! while a menu is posted, so neither can be grabbed without a hand on the mouse. This drives
//! the same `Selection` and `Menu` the desktop does and paints one frame.
//!
//! ```sh
//! cargo run --example band_preview -- ~/Desktop band.pnm        # mid-band
//! cargo run --example band_preview -- ~/Desktop menu.pnm --menu # menu posted
//! ```
use std::path::PathBuf;
use wlrix_desktop::image::Images;
use wlrix_desktop::layout::{Grid, Metrics, Point, Rect};
use wlrix_desktop::menu::{Actions, Menu};
use wlrix_desktop::running::Running;
use wlrix_desktop::select::Selection;
use wlrix_desktop::state::State;
use wlrix_desktop::theme::font::{Face, Fonts};
use wlrix_desktop::ui::{canvas::Canvas, paint};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("a desktop directory"));
    let out = args.next().unwrap_or_else(|| "band.pnm".into());
    let show_menu = args.next().as_deref() == Some("--menu");
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

    // The menu offers a launcher's `[Desktop Action …]` groups when the band happened to pick
    // out exactly one launcher that has some -- the same rule the real desktop uses.
    let lone_launcher = match selection.selected() {
        [name] => entries
            .iter()
            .find(|entry| &entry.name == name)
            .and_then(|entry| entry.launcher.as_ref())
            .filter(|launcher| !launcher.actions.is_empty())
            .map(|launcher| launcher.actions.clone()),
        _ => None,
    };

    // ...and, for the menu, post it over the middle with a row pointed at.
    let menu = show_menu.then(|| {
        let actions = lone_launcher.as_ref().map(|items| Actions {
            entry: "preview",
            items,
        });
        let mut menu = Menu::new(
            Point::new(60, 40),
            selection.selected().len(),
            actions,
            |label| fonts.width(Face::Bold, wlrix_desktop::menu::LABEL_PX, label),
        );
        let row = menu.row(3);
        menu.hover(Point::new(row.x + row.w / 2, row.y + row.h / 2));
        menu
    });
    if show_menu {
        selection.release(&placed, &grid, false);
    }

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
                menu: menu.as_ref(),
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
