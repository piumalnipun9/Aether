// SPDX-License-Identifier: GPL-3.0-or-later

//! Loading the wallpaper and deriving a Material You palette from it.

use std::{
    env, fs,
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
};

use image::{DynamicImage, ImageReader, RgbaImage, imageops};
use material_colors::{
    color::Argb,
    hct::Hct,
    quantize::{Quantizer, QuantizerCelebi},
    scheme::Scheme,
    score::Score,
    theme::ThemeBuilder,
};

use crate::render::border::BorderColors;

/// The longest side of the image colors are extracted from.
const PALETTE_IMAGE_SIZE: u32 = 128;

const MAX_PALETTE_COLORS: usize = 128;

/// What the brightest channel of a vivid pixel reaches at least.
const MIN_VIVID_BRIGHTNESS: u8 = 64;
/// How far apart the brightest and darkest channels of a vivid pixel are at least.
const MIN_VIVID_COLORFULNESS: u8 = 48;
/// How many pixels of the thumbnail have to be vivid for only those to count.
const MIN_VIVID_PIXELS: usize = 32;

/// The chroma and tone of [`Palette::accent`].
const ACCENT_CHROMA: f64 = 90.0;
const ACCENT_TONE_DARK: f64 = 72.0;
const ACCENT_TONE_LIGHT: f64 = 45.0;

/// What a theme is built around when there's no color to take from the wallpaper:
/// Material's own purple.
const FALLBACK_SOURCE: Argb = Argb {
    alpha: 255,
    red: 0x67,
    green: 0x50,
    blue: 0xA4,
};

/// How colorful a wallpaper's colors have to be to build a theme around.
const MIN_SOURCE_CHROMA: f64 = 5.0;

pub use aether_common::Rgb;

/// The Material 3 colors the shell uses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub surface: Rgb,
    pub primary: Rgb,
    pub on_primary: Rgb,
    /// As light as `primary`, which is rather pale in a dark theme, but vivid.
    pub accent: Rgb,
    pub on_surface: Rgb,
    pub on_surface_variant: Rgb,
    pub outline_variant: Rgb,
    pub error: Rgb,
}

/// The light and dark versions of a theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palettes {
    /// The color the theme is built around.
    pub source: Argb,
    pub dark: Palette,
    pub light: Palette,
}

impl Palettes {
    pub fn get(&self, dark: bool) -> &Palette {
        if dark { &self.dark } else { &self.light }
    }

    /// The theme for when no wallpaper has a say.
    pub fn fallback() -> Self {
        Self::from_source(FALLBACK_SOURCE)
    }

    fn from_source(source: Argb) -> Self {
        let schemes = ThemeBuilder::with_source(source).build().schemes;
        Self {
            source,
            dark: Palette::new(schemes.dark, source, ACCENT_TONE_DARK),
            light: Palette::new(schemes.light, source, ACCENT_TONE_LIGHT),
        }
    }
}

impl Palette {
    fn new(scheme: Scheme, source: Argb, accent_tone: f64) -> Self {
        let rgb = |color: Argb| [color.red, color.green, color.blue];
        let Scheme {
            surface,
            primary,
            on_primary,
            on_surface,
            on_surface_variant,
            outline_variant,
            error,
            ..
        } = scheme;

        Self {
            surface: rgb(surface),
            primary: rgb(primary),
            on_primary: rgb(on_primary),
            accent: rgb(Hct::from(Hct::new(source).get_hue(), ACCENT_CHROMA, accent_tone).into()),
            on_surface: rgb(on_surface),
            on_surface_variant: rgb(on_surface_variant),
            outline_variant: rgb(outline_variant),
            error: rgb(error),
        }
    }

    /// These colors for the settings app.
    pub fn colors(&self) -> aether_common::Colors {
        aether_common::Colors {
            surface: self.surface,
            primary: self.primary,
            on_primary: self.on_primary,
            accent: self.accent,
            on_surface: self.on_surface,
            on_surface_variant: self.on_surface_variant,
            error: self.error,
        }
    }

    pub fn border_colors(&self) -> BorderColors {
        let color = |[r, g, b]: Rgb| [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0];
        BorderColors {
            focused: color(self.primary),
            unfocused: color(self.outline_variant),
        }
    }
}

/// Whether a color stands out rather than being part of a dark or gray backdrop.
fn is_vivid(&image::Rgba([red, green, blue, _]): &image::Rgba<u8>) -> bool {
    let brightest = red.max(green).max(blue);
    let darkest = red.min(green).min(blue);
    brightest >= MIN_VIVID_BRIGHTNESS && brightest - darkest >= MIN_VIVID_COLORFULNESS
}

/// Picks the color an image's theme should be built around.
///
/// In a mostly dark image, the backdrop would outnumber what the image is
/// actually about, so only vivid pixels get a say when there are enough of them.
fn source_color(image: &DynamicImage) -> Argb {
    let thumbnail = image
        .thumbnail(PALETTE_IMAGE_SIZE, PALETTE_IMAGE_SIZE)
        .into_rgba8();

    let vivid = thumbnail
        .pixels()
        .filter(|p| is_vivid(p))
        .copied()
        .collect::<Vec<_>>();
    let pixels = if vivid.len() >= MIN_VIVID_PIXELS {
        vivid
    } else {
        thumbnail.pixels().copied().collect()
    };

    let pixels = pixels
        .into_iter()
        .map(|image::Rgba([red, green, blue, alpha])| Argb {
            alpha,
            red,
            green,
            blue,
        })
        .collect::<Vec<_>>();

    let colors = QuantizerCelebi::quantize(&pixels, MAX_PALETTE_COLORS).color_to_count;
    // A white or gray image has nothing to go on, and scoring would make something up.
    if !colors
        .keys()
        .any(|&color| Hct::new(color).get_chroma() >= MIN_SOURCE_CHROMA)
    {
        return FALLBACK_SOURCE;
    }

    Score::score(&colors, Some(1), None, None)[0]
}

/// Scales `image` to fill `width`x`height`, cropping what doesn't fit.
fn zoom(image: &DynamicImage, width: u32, height: u32) -> RgbaImage {
    let (image_width, image_height) = (image.width(), image.height());

    // Whichever side is too long for the target's aspect ratio gets cropped.
    let scale = f64::max(
        width as f64 / image_width as f64,
        height as f64 / image_height as f64,
    );
    let crop_width = ((width as f64 / scale).round() as u32).min(image_width);
    let crop_height = ((height as f64 / scale).round() as u32).min(image_height);

    let cropped = image.crop_imm(
        (image_width - crop_width) / 2,
        (image_height - crop_height) / 2,
        crop_width,
        crop_height,
    );

    imageops::thumbnail(&cropped.to_rgba8(), width, height)
}

/// Loads the wallpaper at `path`, scaled for an output of `width`x`height` pixels,
/// and a palette if asked to.
///
/// This is slow, so call it on another thread. Decoded wallpapers can be large
/// and aren't kept around, so loading for another output decodes the file again.
/// `contents` of the settings file with `wallpaper` set, which keeps comments intact.
pub fn with_wallpaper(contents: &str, wallpaper: &str) -> String {
    with_setting(contents, "wallpaper", toml::Value::from(wallpaper))
}

/// `contents` of the settings file with `key` set, which keeps comments intact.
pub fn with_setting(contents: &str, key: &str, value: toml::Value) -> String {
    let line = format!("{key} = {value}");
    let is_setting = |text: &&str| {
        let text = text.trim_start();
        text.strip_prefix(key)
            .is_some_and(|rest| rest.trim_start().starts_with('='))
    };

    let mut lines = contents.lines().collect::<Vec<_>>();
    match lines.iter().position(|text| is_setting(text)) {
        Some(index) => lines[index] = &line,
        None => lines.push(&line),
    }

    lines.join("\n") + "\n"
}

/// Where screen-sized copies of wallpapers and their colors are kept.
fn cache_dir() -> Option<PathBuf> {
    let cache = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(env::var_os("HOME")?).join(".cache")))?;
    Some(cache.join("pinnacle/wallpapers"))
}

/// A name for the image at `path`, which changes when the image does.
fn cache_key(path: &Path) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    metadata.len().hash(&mut hasher);
    metadata.modified().ok()?.hash(&mut hasher);
    Some(format!("{:016x}", hasher.finish()))
}

fn read_source(path: &Path) -> Option<Argb> {
    let text = fs::read_to_string(path).ok()?;
    let [red, green, blue] = u32::from_str_radix(text.trim(), 16).ok()?.to_be_bytes()[1..]
        .try_into()
        .ok()?;
    Some(Argb {
        alpha: 255,
        red,
        green,
        blue,
    })
}

/// Writes a file so that nothing ever finds only part of it.
fn write_whole(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let partial = path.with_extension("partial");
    fs::write(&partial, contents)?;
    fs::rename(&partial, path)
}

/// Loads the wallpaper at `path` for an output, cropped to fit it, and hands its colors to
/// `on_palette` if there's a use for them.
///
/// Decoding a big image is slow and takes a lot of memory, and nothing of it is needed but
/// a screen's worth of pixels and one color. So that only has to happen once per image.
pub fn load(
    path: &Path,
    width: u32,
    height: u32,
    with_palette: bool,
    on_palette: impl FnOnce(Palettes),
) -> anyhow::Result<RgbaImage> {
    load_with_cache(
        cache_dir().as_deref(),
        path,
        width,
        height,
        with_palette,
        on_palette,
    )
}

fn load_with_cache(
    cache: Option<&Path>,
    path: &Path,
    width: u32,
    height: u32,
    with_palette: bool,
    on_palette: impl FnOnce(Palettes),
) -> anyhow::Result<RgbaImage> {
    let key = cache.zip(cache_key(path)).map(|(dir, key)| dir.join(key));
    let source_file = key.as_ref().map(|key| key.with_extension("source"));
    let image_file = key
        .as_ref()
        .map(|key| key.with_extension(format!("{width}x{height}.rgba")));

    let mut on_palette = with_palette.then_some(on_palette);

    if on_palette.is_some()
        && let Some(source) = source_file.as_deref().and_then(read_source)
        && let Some(on_palette) = on_palette.take()
    {
        on_palette(Palettes::from_source(source));
    }

    if on_palette.is_none()
        && let Some(image) = image_file
            .as_ref()
            .and_then(|file| fs::read(file).ok())
            .and_then(|pixels| RgbaImage::from_raw(width, height, pixels))
    {
        return Ok(image);
    }

    let image = ImageReader::open(path)?.with_guessed_format()?.decode()?;
    let source = source_color(&image);

    if let Some(on_palette) = on_palette.take() {
        on_palette(Palettes::from_source(source));
    }

    let zoomed = zoom(&image, width, height);
    drop(image);

    // Not being able to keep these only makes the next start slower.
    if let (Some(source_file), Some(image_file)) = (&source_file, &image_file) {
        let written = source_file
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| {
                let color = format!(
                    "{:02x}{:02x}{:02x}\n",
                    source.red, source.green, source.blue
                );
                write_whole(source_file, color.as_bytes())?;
                write_whole(image_file, zoomed.as_raw())
            });
        if let Err(err) = written {
            tracing::warn!("Failed to keep a copy of the wallpaper: {err}");
        }
    }

    Ok(zoomed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saving_the_wallpaper_keeps_the_rest() {
        let contents = "# A comment.\nwallpaper = \"~/a.jpg\"\nother = 1\n";
        assert_eq!(
            with_wallpaper(contents, "/b.jpg"),
            "# A comment.\nwallpaper = \"/b.jpg\"\nother = 1\n"
        );
        assert_eq!(with_wallpaper("", "/b.jpg"), "wallpaper = \"/b.jpg\"\n");
        assert_eq!(
            with_setting("blur = true\nblurry = 1\n", "blur", false.into()),
            "blur = false\nblurry = 1\n"
        );
    }

    #[test]
    fn the_cache_saves_decoding() {
        let dir = env::temp_dir().join(format!("pinnacle-wallpaper-{}", std::process::id()));
        let cache = dir.join("cache");
        fs::create_dir_all(&dir).unwrap();

        let path = dir.join("wallpaper.png");
        image::RgbImage::from_pixel(64, 64, image::Rgb([200, 40, 30]))
            .save(&path)
            .unwrap();

        let palette = std::cell::Cell::new(None);
        let load = |width, height, with_palette| {
            load_with_cache(Some(&cache), &path, width, height, with_palette, |found| {
                palette.set(Some(found));
            })
            .unwrap()
        };

        let first = load(16, 9, true);
        let colors = palette.take().unwrap();

        // Only what's in the cache could come out like this.
        let key = cache.join(cache_key(&path).unwrap());
        let file = key.with_extension("16x9.rgba");
        fs::write(&file, vec![7; first.as_raw().len()]).unwrap();

        assert_eq!(load(16, 9, true).as_raw()[0], 7);
        assert_eq!(palette.take(), Some(colors));

        // Nothing asks for colors, so there's nothing to hand over.
        load(16, 9, false);
        assert_eq!(palette.take(), None);

        // Another size needs its own copy, but not its own colors.
        assert_eq!(load(8, 8, true).dimensions(), (8, 8));
        assert_eq!(palette.take(), Some(colors));

        // A copy of the wrong size isn't used.
        fs::write(&file, [7; 4]).unwrap();
        assert_eq!(load(16, 9, false).as_raw()[0], first.as_raw()[0]);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn zoom_covers_the_output() {
        let wide = DynamicImage::new_rgba8(400, 100);
        assert_eq!(zoom(&wide, 100, 100).dimensions(), (100, 100));

        let tall = DynamicImage::new_rgba8(100, 400);
        assert_eq!(zoom(&tall, 200, 100).dimensions(), (200, 100));
    }

    /// An image that's mostly `backdrop` with a small patch of `accent`.
    fn image_with_accent(backdrop: [u8; 3], accent: [u8; 3]) -> DynamicImage {
        let [r, g, b] = backdrop;
        let mut image = RgbaImage::from_pixel(100, 100, image::Rgba([r, g, b, 255]));
        let [r, g, b] = accent;
        for pixel in image.pixels_mut().take(500) {
            *pixel = image::Rgba([r, g, b, 255]);
        }
        DynamicImage::ImageRgba8(image)
    }

    #[test]
    fn a_small_accent_beats_a_dark_backdrop() {
        // Dark navy with a 5% red glow, like a night scene.
        let image = image_with_accent([14, 15, 26], [200, 20, 20]);
        let [red, green, blue] = Palettes::from_source(source_color(&image)).dark.primary;
        assert!(red > green && red > blue, "{:?}", [red, green, blue]);
    }

    #[test]
    fn plain_images_get_the_fallback_theme() {
        for color in [[255, 255, 255], [128, 128, 128], [0, 0, 0]] {
            let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(
                64,
                64,
                image::Rgba([color[0], color[1], color[2], 255]),
            ));
            assert_eq!(source_color(&image), FALLBACK_SOURCE);
        }
    }

    #[test]
    fn images_without_anything_vivid_still_get_a_palette() {
        let image = image_with_accent([14, 15, 26], [20, 20, 30]);
        Palettes::from_source(source_color(&image));
    }
}
