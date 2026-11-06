// SPDX-License-Identifier: GPL-3.0-or-later

//! Quick settings' tiles: which ones there are, how they're arranged, and editing that.

use std::{fs, path::Path};

use serde::{Deserialize, Serialize};
use tracing::warn;

/// The file the tiles are kept in, in the config directory.
const TILES_FILE: &str = "quick-settings.toml";

const COLUMNS: u32 = 4;

/// How many rows of tiles fit on a page. The rest go on the next one.
const ROWS_PER_PAGE: u32 = 3;

/// Where a tile goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Place {
    pub page: u32,
    pub column: u32,
    pub row: u32,
}

/// How the tiles are arranged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arrangement {
    /// For each tile, in order.
    pub places: Vec<Place>,
    pub pages: u32,
    /// How many rows the tiles take up on a page.
    pub rows: u32,
}

/// Each is `TileItem.kind` in the UI, where the icons are, so the order matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TileKind {
    Wifi,
    Bluetooth,
    Airplane,
    Dark,
    NightLight,
    PowerSaver,
    StayAwake,
    Microphone,
    Audio,
    DoNotDisturb,
}

impl TileKind {
    pub const ALL: [Self; 10] = [
        Self::Wifi,
        Self::Bluetooth,
        Self::Airplane,
        Self::Dark,
        Self::NightLight,
        Self::PowerSaver,
        Self::StayAwake,
        Self::Microphone,
        Self::Audio,
        Self::DoNotDisturb,
    ];

    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|kind| *kind == self)
            .expect("it's in there")
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Wifi => "Wi-Fi",
            Self::Bluetooth => "Bluetooth",
            Self::Airplane => "Airplane mode",
            Self::Dark => "Dark theme",
            Self::NightLight => "Night Light",
            Self::PowerSaver => "Battery Saver",
            Self::StayAwake => "Stay awake",
            Self::Microphone => "Microphone",
            Self::Audio => "Audio",
            Self::DoNotDisturb => "Do Not Disturb",
        }
    }

    /// Whether the tile has a page, and so is two buttons when it's large.
    pub fn has_page(self) -> bool {
        matches!(
            self,
            Self::Wifi | Self::Bluetooth | Self::Microphone | Self::Audio
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TileConfig {
    pub kind: TileKind,
    pub large: bool,
}

/// The tiles being shown, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tiles {
    pub tiles: Vec<TileConfig>,
}

impl Default for Tiles {
    fn default() -> Self {
        use TileKind::*;

        let tile = |kind, large| TileConfig { kind, large };
        Self {
            tiles: vec![
                tile(Wifi, true),
                tile(Bluetooth, true),
                tile(Audio, true),
                tile(Dark, true),
                tile(NightLight, true),
                tile(PowerSaver, true),
                tile(Airplane, false),
                tile(StayAwake, false),
                tile(Microphone, false),
                tile(DoNotDisturb, false),
            ],
        }
    }
}

impl Tiles {
    /// Reads the tiles from `config_dir`. Without a valid file, these are the defaults.
    pub fn read(config_dir: &Path) -> Self {
        let path = config_dir.join(TILES_FILE);
        let Ok(contents) = fs::read_to_string(&path) else {
            return Self::default();
        };

        let mut tiles = toml::from_str::<Self>(&contents).unwrap_or_else(|err| {
            warn!("Failed to parse {}: {err}", path.display());
            Self::default()
        });

        // A tile can't be in two places.
        let mut seen = Vec::new();
        tiles.tiles.retain(|tile| {
            let new = !seen.contains(&tile.kind);
            seen.push(tile.kind);
            new
        });

        tiles
    }

    pub fn write(&self, config_dir: &Path) {
        let path = config_dir.join(TILES_FILE);
        let result = toml::to_string(self)
            .map_err(anyhow::Error::from)
            .and_then(|contents| Ok(fs::write(&path, contents)?));
        if let Err(err) = result {
            warn!("Failed to write {}: {err}", path.display());
        }
    }

    pub fn get(&self, kind: TileKind) -> Option<&TileConfig> {
        self.tiles.iter().find(|tile| tile.kind == kind)
    }

    pub fn toggle_size(&mut self, kind: TileKind) {
        if let Some(tile) = self.tiles.iter_mut().find(|tile| tile.kind == kind) {
            tile.large = !tile.large;
        }
    }

    pub fn remove(&mut self, kind: TileKind) {
        self.tiles.retain(|tile| tile.kind != kind);
    }

    pub fn add(&mut self, kind: TileKind) {
        if self.get(kind).is_none() {
            self.tiles.push(TileConfig { kind, large: false });
        }
    }

    /// Puts `kind` where `target` is.
    pub fn move_to(&mut self, kind: TileKind, target: TileKind) {
        let from = self.tiles.iter().position(|tile| tile.kind == kind);
        let to = self.tiles.iter().position(|tile| tile.kind == target);
        if let (Some(from), Some(to)) = (from, to) {
            let tile = self.tiles.remove(from);
            self.tiles.insert(to, tile);
        }
    }

    /// Arranges the tiles on pages of `rows_per_page` rows, or on one page without a limit.
    ///
    /// Tiles fill the rows in order. A large tile that doesn't fit starts a new row,
    /// and what's left at the end of the previous one stays empty.
    fn arrange_with(&self, rows_per_page: Option<u32>) -> Arrangement {
        let (mut page, mut column, mut row) = (0, 0, 0);
        let mut most_rows = 1;

        let places = self
            .tiles
            .iter()
            .map(|tile| {
                let width = if tile.large { 2 } else { 1 };
                if column + width > COLUMNS {
                    column = 0;
                    row += 1;
                    if rows_per_page.is_some_and(|rows| row >= rows) {
                        row = 0;
                        page += 1;
                    }
                }
                let place = Place { page, column, row };
                column += width;
                most_rows = most_rows.max(row + 1);
                place
            })
            .collect();

        Arrangement {
            places,
            pages: page + 1,
            rows: most_rows,
        }
    }

    /// How the tiles are arranged normally.
    pub fn arrange(&self) -> Arrangement {
        self.arrange_with(Some(ROWS_PER_PAGE))
    }

    /// How the tiles are arranged while editing, which is all together.
    pub fn arrange_for_editing(&self) -> Arrangement {
        self.arrange_with(None)
    }

    /// Where the `index`th tile that isn't in use goes, below the others.
    pub fn hidden_place(index: u32) -> (u32, u32) {
        (index % COLUMNS, index / COLUMNS)
    }

    /// How many rows the tiles that aren't in use take up.
    pub fn hidden_rows(&self) -> u32 {
        (TileKind::ALL.len() - self.tiles.len()).div_ceil(COLUMNS as usize) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::{TileKind::*, *};

    fn tiles(list: &[(TileKind, bool)]) -> Tiles {
        Tiles {
            tiles: list
                .iter()
                .map(|&(kind, large)| TileConfig { kind, large })
                .collect(),
        }
    }

    fn places(arrangement: &Arrangement) -> Vec<(u32, u32, u32)> {
        arrangement
            .places
            .iter()
            .map(|place| (place.page, place.column, place.row))
            .collect()
    }

    #[test]
    fn large_tiles_wrap() {
        let tiles = tiles(&[
            (Wifi, false),
            (Bluetooth, true),
            (Dark, true),
            (Airplane, false),
        ]);
        let arrangement = tiles.arrange();
        assert_eq!(
            places(&arrangement),
            [(0, 0, 0), (0, 1, 0), (0, 0, 1), (0, 2, 1)]
        );
        assert_eq!(arrangement.rows, 2);
        assert_eq!(arrangement.pages, 1);
    }

    #[test]
    fn a_full_row_starts_a_new_one() {
        let arrangement = tiles(&[(Wifi, true), (Dark, true), (Airplane, false)]).arrange();
        assert_eq!(places(&arrangement), [(0, 0, 0), (0, 2, 0), (0, 0, 1)]);
        assert_eq!(arrangement.rows, 2);
    }

    #[test]
    fn no_tiles_take_one_row() {
        assert_eq!(tiles(&[]).arrange().rows, 1);
    }

    #[test]
    fn tiles_past_a_full_page_go_on_the_next_one() {
        let arrangement = Tiles::default().arrange();
        assert_eq!(arrangement.pages, 2);
        assert_eq!(arrangement.rows, 3);
        assert_eq!(
            arrangement.places[6],
            Place {
                page: 1,
                column: 0,
                row: 0
            }
        );
    }

    #[test]
    fn editing_keeps_all_tiles_together() {
        let arrangement = Tiles::default().arrange_for_editing();
        assert_eq!(arrangement.pages, 1);
        assert_eq!(arrangement.rows, 4);
    }

    #[test]
    fn hidden_tiles_get_rows_of_their_own() {
        let mut tiles = Tiles::default();
        assert_eq!(tiles.hidden_rows(), 0);
        tiles.remove(Wifi);
        assert_eq!(tiles.hidden_rows(), 1);
        assert_eq!(Tiles::hidden_place(5), (1, 1));
    }

    #[test]
    fn resizing_and_moving() {
        let mut tiles = tiles(&[(Wifi, true), (Dark, false), (Airplane, false)]);
        tiles.toggle_size(Dark);
        assert!(tiles.get(Dark).unwrap().large);

        tiles.move_to(Airplane, Wifi);
        assert_eq!(tiles.tiles[0].kind, Airplane);
        assert_eq!(tiles.tiles[1].kind, Wifi);

        tiles.remove(Wifi);
        assert!(tiles.get(Wifi).is_none());
        tiles.add(Wifi);
        tiles.add(Wifi);
        assert_eq!(tiles.tiles.len(), 3);
    }

    #[test]
    fn survives_being_saved() {
        let original = Tiles::default();
        let text = toml::to_string(&original).unwrap();
        assert!(text.contains("night-light"));
        assert_eq!(toml::from_str::<Tiles>(&text).unwrap(), original);
    }
}
