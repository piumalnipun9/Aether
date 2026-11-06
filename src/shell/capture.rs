// SPDX-License-Identifier: GPL-3.0-or-later

//! Turning a capture of the screen into a file.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Local};
use image::{RgbaImage, imageops};

/// A selection smaller than this in pixels is a click.
const MIN_SELECTION: u32 = 4;

/// A part of an output in logical pixels, as x, y, width and height.
pub type Area = (f64, f64, f64, f64);

/// What `area` of an output is in pixels of an image `width`x`height`, as x, y, width and
/// height. That's all of it for an area too small to have been meant.
pub fn pixel_area(area: Option<Area>, scale: f64, width: u32, height: u32) -> (u32, u32, u32, u32) {
    let whole = (0, 0, width, height);
    let Some((area_x, area_y, area_width, area_height)) = area else {
        return whole;
    };

    let x = ((area_x * scale).round().max(0.0) as u32).min(width);
    let y = ((area_y * scale).round().max(0.0) as u32).min(height);
    let cut_width = ((area_width * scale).round() as u32).min(width - x);
    let cut_height = ((area_height * scale).round() as u32).min(height - y);

    if cut_width < MIN_SELECTION || cut_height < MIN_SELECTION {
        whole
    } else {
        (x, y, cut_width, cut_height)
    }
}

/// The name of a screenshot taken at `time`.
fn file_name(time: DateTime<Local>) -> String {
    format!("Screenshot from {}.png", time.format("%Y-%m-%d %H-%M-%S"))
}

/// Saves `area` of `image` into `dir`, and gives back the file.
fn save_to(dir: &Path, image: &RgbaImage, area: (u32, u32, u32, u32)) -> anyhow::Result<PathBuf> {
    fs::create_dir_all(dir)?;

    let (x, y, width, height) = area;
    let path = dir.join(file_name(Local::now()));
    imageops::crop_imm(image, x, y, width, height)
        .to_image()
        .save(&path)?;
    Ok(path)
}

/// Saves `area` of `image` where screenshots go. This is slow, so call it on another thread.
pub fn save(image: &RgbaImage, area: (u32, u32, u32, u32)) -> anyhow::Result<PathBuf> {
    let home = env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME isn't set"))?;
    save_to(
        &PathBuf::from(home).join("Pictures/Screenshots"),
        image,
        area,
    )
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Option<Area> {
        Some((x, y, width, height))
    }

    #[test]
    fn selections_become_pixels() {
        assert_eq!(
            pixel_area(rect(10.0, 20.0, 30.0, 40.0), 1.0, 100, 100),
            (10, 20, 30, 40)
        );
        assert_eq!(
            pixel_area(rect(10.0, 20.0, 30.0, 40.0), 1.5, 100, 100),
            (15, 30, 45, 60)
        );
    }

    #[test]
    fn selections_stay_on_the_image() {
        assert_eq!(
            pixel_area(rect(90.0, 90.0, 50.0, 50.0), 1.0, 100, 100),
            (90, 90, 10, 10)
        );
    }

    #[test]
    fn clicks_take_everything() {
        assert_eq!(
            pixel_area(rect(50.0, 50.0, 0.0, 0.0), 1.0, 100, 80),
            (0, 0, 100, 80)
        );
        assert_eq!(pixel_area(None, 2.0, 100, 80), (0, 0, 100, 80));
    }

    #[test]
    fn names_sort_by_time() {
        let time = Local.with_ymd_and_hms(2026, 11, 6, 9, 5, 3).unwrap();
        assert_eq!(file_name(time), "Screenshot from 2026-11-06 09-05-03.png");
    }

    #[test]
    fn the_selection_is_what_gets_saved() {
        let dir = env::temp_dir().join(format!("aether-shot-{}", std::process::id()));
        let mut image = RgbaImage::new(10, 10);
        image.put_pixel(3, 4, image::Rgba([255, 0, 0, 255]));

        let path = save_to(&dir, &image, (3, 4, 2, 2)).unwrap();
        let saved = image::open(&path).unwrap().into_rgba8();

        assert_eq!(saved.dimensions(), (2, 2));
        assert_eq!(saved.get_pixel(0, 0), &image::Rgba([255, 0, 0, 255]));
        fs::remove_dir_all(dir).unwrap();
    }
}
