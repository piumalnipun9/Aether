// SPDX-License-Identifier: GPL-3.0-or-later

//! Sharing the wallpaper's theme with other apps.
//!
//! `aether/colors.json` has every Material role for both the dark and light theme, for
//! anything that wants to read them, and `aether/colors.css` the same as GTK named colors.
//! Apps with a theme format of their own get a file in it: GTK, foot, kitty and Qt.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use material_colors::{color::Argb, hct::Hct, theme::ThemeBuilder};
use serde_json::{Map, Value, json};

use super::wallpaper::{Palettes, Rgb};

/// What marks a `gtk.css` as ours to replace.
const MARKER: &str = "/* Written by Aether";

const HEADER: &str =
    "/* Written by Aether from the wallpaper's colors. Changes here get replaced. */\n";

/// How see-through terminals are when what's behind them is blurred.
const TERMINAL_ALPHA: f64 = 0.82;

/// The hues of the terminal colors, from red to cyan in the order of the ANSI ones.
const ANSI_HUES: [f64; 6] = [25.0, 145.0, 90.0, 265.0, 320.0, 200.0];
/// How far terminal colors move towards the theme's own.
const ANSI_HARMONY: f64 = 0.15;
const ANSI_CHROMA: f64 = 55.0;

type Roles = Vec<(&'static str, Rgb)>;

fn roles(source: Argb, dark: bool) -> Roles {
    let schemes = ThemeBuilder::with_source(source).build().schemes;
    let s = if dark { schemes.dark } else { schemes.light };
    let rgb = |color: Argb| [color.red, color.green, color.blue];

    macro_rules! roles {
        ($($name:ident),* $(,)?) => {
            vec![$((stringify!($name), rgb(s.$name))),*]
        };
    }

    roles![
        primary,
        on_primary,
        primary_container,
        on_primary_container,
        inverse_primary,
        secondary,
        on_secondary,
        secondary_container,
        on_secondary_container,
        tertiary,
        on_tertiary,
        tertiary_container,
        on_tertiary_container,
        error,
        on_error,
        error_container,
        on_error_container,
        surface_dim,
        surface,
        surface_bright,
        surface_container_lowest,
        surface_container_low,
        surface_container,
        surface_container_high,
        surface_container_highest,
        on_surface,
        on_surface_variant,
        outline,
        outline_variant,
        inverse_surface,
        inverse_on_surface,
        surface_variant,
        background,
        on_background,
    ]
}

fn role(roles: &Roles, name: &str) -> Rgb {
    roles
        .iter()
        .find(|(role, _)| *role == name)
        .map_or([0, 0, 0], |(_, color)| *color)
}

fn hex([red, green, blue]: Rgb) -> String {
    format!("#{red:02x}{green:02x}{blue:02x}")
}

/// `hue` moved a bit towards `towards`, the short way around.
fn harmonize(hue: f64, towards: f64) -> f64 {
    let difference = (towards - hue + 540.0).rem_euclid(360.0) - 180.0;
    (hue + difference * ANSI_HARMONY).rem_euclid(360.0)
}

/// The 16 terminal colors.
fn ansi(source: Argb, dark: bool, roles: &Roles) -> [Rgb; 16] {
    let source_hue = Hct::new(source).get_hue();
    let color = |hue: f64, tone: f64| {
        let color: Argb = Hct::from(harmonize(hue, source_hue), ANSI_CHROMA, tone).into();
        [color.red, color.green, color.blue]
    };
    let (normal, bright) = if dark { (72.0, 84.0) } else { (40.0, 32.0) };

    let mut colors = [[0; 3]; 16];
    colors[0] = role(roles, "surface_container_highest");
    colors[7] = role(roles, "on_surface_variant");
    colors[8] = role(roles, "outline");
    colors[15] = role(roles, "on_surface");
    for (index, hue) in ANSI_HUES.into_iter().enumerate() {
        colors[index + 1] = color(hue, normal);
        colors[index + 9] = color(hue, bright);
    }
    colors
}

fn colors_json(palettes: &Palettes, dark: bool) -> String {
    let map = |roles: Roles| {
        roles
            .into_iter()
            .map(|(name, color)| (name.to_owned(), Value::from(hex(color))))
            .collect::<Map<_, _>>()
    };
    let source = palettes.source;
    let (dark_roles, light_roles) = (roles(source, true), roles(source, false));
    let (dark_ansi, light_ansi) = (
        ansi(source, true, &dark_roles),
        ansi(source, false, &light_roles),
    );
    // The terminal colors, in the format Dank Material Shell shares them in.
    let terminal = dark_ansi
        .into_iter()
        .zip(light_ansi)
        .enumerate()
        .map(|(index, (dark, light))| {
            (
                format!("color{index}"),
                json!({ "dark": hex(dark), "light": hex(light) }),
            )
        })
        .collect::<Map<_, _>>();

    let json = json!({
        "mode": if dark { "dark" } else { "light" },
        "source": hex([source.red, source.green, source.blue]),
        "colors": {
            "dark": map(dark_roles),
            "light": map(light_roles),
        },
        "dank16": terminal,
    });
    serde_json::to_string_pretty(&json).unwrap_or_default() + "\n"
}

fn colors_css(roles: &Roles) -> String {
    let mut css = String::from(HEADER);
    for (name, color) in roles {
        css += &format!("@define-color {name} {};\n", hex(*color));
    }
    css
}

/// The colors libadwaita and GTK 4's own theme go by.
fn gtk4_css(roles: &Roles) -> String {
    let color = |name| hex(role(roles, name));
    let defines = [
        ("accent_color", "primary"),
        ("accent_bg_color", "primary"),
        ("accent_fg_color", "on_primary"),
        ("destructive_color", "error"),
        ("destructive_bg_color", "error"),
        ("destructive_fg_color", "on_error"),
        ("error_color", "error"),
        ("error_bg_color", "error"),
        ("error_fg_color", "on_error"),
        ("window_bg_color", "surface"),
        ("window_fg_color", "on_surface"),
        ("view_bg_color", "surface_container_low"),
        ("view_fg_color", "on_surface"),
        ("headerbar_bg_color", "surface_container"),
        ("headerbar_fg_color", "on_surface"),
        ("headerbar_border_color", "outline_variant"),
        ("headerbar_backdrop_color", "surface"),
        ("card_bg_color", "surface_container"),
        ("card_fg_color", "on_surface"),
        ("popover_bg_color", "surface_container_high"),
        ("popover_fg_color", "on_surface"),
        ("dialog_bg_color", "surface_container_high"),
        ("dialog_fg_color", "on_surface"),
    ];

    let mut css = String::from(HEADER);
    for (name, role) in defines {
        css += &format!("@define-color {name} {};\n", color(role));
    }
    css += &format!(
        ".navigation-sidebar, .sidebar {{\n    background-color: {};\n    color: {};\n}}\n",
        color("surface_container"),
        color("on_surface"),
    );
    css
}

/// GTK 3 themes are mostly fixed colors, so this has to go after the common widgets.
fn gtk3_css(roles: &Roles) -> String {
    let color = |name| hex(role(roles, name));
    let defines = [
        ("theme_bg_color", "surface"),
        ("theme_fg_color", "on_surface"),
        ("theme_base_color", "surface_container_low"),
        ("theme_text_color", "on_surface"),
        ("theme_selected_bg_color", "primary"),
        ("theme_selected_fg_color", "on_primary"),
        ("theme_unfocused_bg_color", "surface"),
        ("theme_unfocused_fg_color", "on_surface_variant"),
        ("borders", "outline_variant"),
    ];

    let mut css = String::from(HEADER);
    for (name, role) in defines {
        css += &format!("@define-color {name} {};\n", color(role));
    }
    css += &format!(
        "\
window, .background {{
    background-color: @theme_bg_color;
    color: @theme_fg_color;
}}
headerbar, .titlebar, toolbar, .toolbar, menubar {{
    background-color: {header};
    background-image: none;
    color: @theme_fg_color;
    border-color: @borders;
}}
.sidebar, .sidebar .view, .sidebar list {{
    background-color: {header};
    color: @theme_fg_color;
}}
.view, iconview, treeview.view, textview text, list {{
    background-color: @theme_base_color;
    color: @theme_text_color;
}}
.view:selected, iconview:selected, treeview.view:selected, list row:selected, selection {{
    background-color: @theme_selected_bg_color;
    color: @theme_selected_fg_color;
}}
",
        header = color("surface_container"),
    );
    css
}

fn foot_ini(source: Argb, dark: bool, roles: &Roles, blur: bool) -> String {
    let plain = |color: Rgb| hex(color)[1..].to_owned();
    let ansi = ansi(source, dark, roles);

    let mut ini = String::from("# Written by Aether from the wallpaper's colors.\n[colors]\n");
    ini += &format!("alpha={}\n", if blur { TERMINAL_ALPHA } else { 1.0 });
    ini += &format!("background={}\n", plain(role(roles, "surface")));
    ini += &format!("foreground={}\n", plain(role(roles, "on_surface")));
    ini += &format!("selection-background={}\n", plain(role(roles, "primary")));
    ini += &format!(
        "selection-foreground={}\n",
        plain(role(roles, "on_primary"))
    );
    for (index, color) in ansi.into_iter().enumerate() {
        let (kind, number) = if index < 8 {
            ("regular", index)
        } else {
            ("bright", index - 8)
        };
        ini += &format!("{kind}{number}={}\n", plain(color));
    }
    ini += &format!(
        "[cursor]\ncolor={} {}\n",
        plain(role(roles, "on_primary")),
        plain(role(roles, "primary"))
    );
    ini
}

fn kitty_conf(source: Argb, dark: bool, roles: &Roles, blur: bool) -> String {
    let color = |name| hex(role(roles, name));
    let mut conf = String::from("# Written by Aether from the wallpaper's colors.\n");
    conf += &format!(
        "background_opacity {}\n",
        if blur { TERMINAL_ALPHA } else { 1.0 }
    );
    for (key, name) in [
        ("background", "surface"),
        ("foreground", "on_surface"),
        ("selection_background", "primary"),
        ("selection_foreground", "on_primary"),
        ("cursor", "primary"),
        ("cursor_text_color", "on_primary"),
        ("active_tab_background", "primary"),
        ("active_tab_foreground", "on_primary"),
        ("inactive_tab_background", "surface_container"),
        ("inactive_tab_foreground", "on_surface_variant"),
    ] {
        conf += &format!("{key} {}\n", color(name));
    }
    for (index, value) in ansi(source, dark, roles).into_iter().enumerate() {
        conf += &format!("color{index} {}\n", hex(value));
    }
    conf
}

/// A color scheme for qt5ct and qt6ct.
fn qt_scheme(roles: &Roles) -> String {
    let color = |name| hex(role(roles, name));
    // In the order of QPalette's color roles.
    let active = [
        "on_surface",
        "surface_container",
        "surface_container_highest",
        "surface_container_high",
        "surface_dim",
        "surface_container_low",
        "on_surface",
        "on_surface",
        "on_surface",
        "surface_container_low",
        "surface",
        "surface_container_lowest",
        "primary",
        "on_primary",
        "primary",
        "tertiary",
        "surface_container",
        "surface",
        "surface_container_high",
        "on_surface",
        "on_surface_variant",
    ]
    .map(color)
    .join(", ");
    let disabled = [("on_surface", "outline"), ("on_surface_variant", "outline")]
        .iter()
        .fold(active.clone(), |text, (from, to)| {
            text.replace(&color(from), &color(to))
        });

    format!(
        "[ColorScheme]\nactive_colors={active}\ndisabled_colors={disabled}\n\
         inactive_colors={active}\n"
    )
}

/// `contents` of a GTK settings file with the dark theme set to `dark`.
fn with_gtk_dark(contents: &str, dark: bool) -> String {
    const KEY: &str = "gtk-application-prefer-dark-theme";
    let line = format!("{KEY}={}", u8::from(dark));

    let mut lines = contents.lines().collect::<Vec<_>>();
    if let Some(index) = lines.iter().position(|text| text.starts_with(KEY)) {
        lines[index] = &line;
    } else {
        if lines.is_empty() {
            lines.push("[Settings]");
        }
        lines.push(&line);
    }
    lines.join("\n") + "\n"
}

/// Writes `path` if that changes anything, as there may be apps watching it.
fn write(path: &Path, contents: &str) -> std::io::Result<()> {
    if fs::read_to_string(path).is_ok_and(|current| current == contents) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)
}

/// Writes a `gtk.css`. One that's not ours gets kept next to it, once.
fn write_gtk_css(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Ok(current) = fs::read_to_string(path)
        && !current.starts_with(MARKER)
    {
        let backup = path.with_extension("css.before-aether");
        if !backup.exists() {
            fs::write(backup, current)?;
        }
    }
    write(path, contents)
}

fn export_to(config: &Path, palettes: &Palettes, dark: bool, blur: bool) -> std::io::Result<()> {
    let source = palettes.source;
    let roles = roles(source, dark);

    write(
        &config.join("aether/colors.json"),
        &colors_json(palettes, dark),
    )?;
    write(&config.join("aether/colors.css"), &colors_css(&roles))?;

    write_gtk_css(&config.join("gtk-4.0/gtk.css"), &gtk4_css(&roles))?;
    write_gtk_css(&config.join("gtk-3.0/gtk.css"), &gtk3_css(&roles))?;
    for dir in ["gtk-3.0", "gtk-4.0"] {
        let path = config.join(dir).join("settings.ini");
        let contents = fs::read_to_string(&path).unwrap_or_default();
        write(&path, &with_gtk_dark(&contents, dark))?;
    }

    write(
        &config.join("foot/aether-colors.ini"),
        &foot_ini(source, dark, &roles, blur),
    )?;
    write(
        &config.join("kitty/aether-colors.conf"),
        &kitty_conf(source, dark, &roles, blur),
    )?;
    for dir in ["qt5ct", "qt6ct"] {
        write(
            &config.join(dir).join("colors/Aether.conf"),
            &qt_scheme(&roles),
        )?;
    }

    Ok(())
}

fn config_home() -> Option<PathBuf> {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(env::var_os("HOME")?).join(".config")))
}

/// Shares the theme. This touches a lot of files, so call it on another thread.
pub fn export(palettes: &Palettes, dark: bool, blur: bool) {
    let Some(config) = config_home() else {
        return;
    };
    if let Err(err) = export_to(&config, palettes, dark, blur) {
        tracing::warn!("Failed to share the theme: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("aether-export-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn it_all_gets_written() {
        let dir = temp_dir("all");
        export_to(&dir, &Palettes::fallback(), true, true).unwrap();

        let json: Value =
            serde_json::from_str(&fs::read_to_string(dir.join("aether/colors.json")).unwrap())
                .unwrap();
        assert_eq!(json["mode"], "dark");
        assert!(json["colors"]["light"]["surface_container_high"].is_string());
        assert!(json["dank16"]["color15"]["dark"].is_string());

        let foot = fs::read_to_string(dir.join("foot/aether-colors.ini")).unwrap();
        assert!(foot.contains("alpha=0.82"));
        assert!(foot.contains("regular0=") && foot.contains("bright7="));

        let settings = fs::read_to_string(dir.join("gtk-4.0/settings.ini")).unwrap();
        assert_eq!(
            settings,
            "[Settings]\ngtk-application-prefer-dark-theme=1\n"
        );

        // Qt wants exactly this many colors.
        let qt = fs::read_to_string(dir.join("qt5ct/colors/Aether.conf")).unwrap();
        let active = qt.lines().nth(1).unwrap();
        assert_eq!(active.split(", ").count(), 21);

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn terminals_are_solid_without_blur() {
        let dir = temp_dir("solid");
        export_to(&dir, &Palettes::fallback(), false, false).unwrap();
        let foot = fs::read_to_string(dir.join("foot/aether-colors.ini")).unwrap();
        assert!(foot.contains("alpha=1\n"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn someone_elses_gtk_css_is_kept_once() {
        let dir = temp_dir("css");
        let css = dir.join("gtk.css");

        fs::write(&css, "/* mine */\n").unwrap();
        write_gtk_css(&css, &format!("{HEADER}a\n")).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("gtk.css.before-aether")).unwrap(),
            "/* mine */\n"
        );

        // Our own doesn't replace the backup.
        write_gtk_css(&css, &format!("{HEADER}b\n")).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("gtk.css.before-aether")).unwrap(),
            "/* mine */\n"
        );
        assert_eq!(fs::read_to_string(&css).unwrap(), format!("{HEADER}b\n"));

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_dark_setting_changes_only_its_line() {
        assert_eq!(
            with_gtk_dark(
                "[Settings]\nother=1\ngtk-application-prefer-dark-theme=0\n",
                true
            ),
            "[Settings]\nother=1\ngtk-application-prefer-dark-theme=1\n"
        );
    }

    #[test]
    fn hues_go_the_short_way_around() {
        assert!((harmonize(350.0, 10.0) - 353.0).abs() < 0.001);
        assert!((harmonize(10.0, 350.0) - 7.0).abs() < 0.001);
    }
}
