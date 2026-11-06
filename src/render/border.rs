// SPDX-License-Identifier: GPL-3.0-or-later

//! Window borders, drawn by the compositor.

use smithay::{
    backend::renderer::element::{
        Kind,
        solid::{SolidColorBuffer, SolidColorRenderElement},
    },
    desktop::WindowSurface,
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel,
    utils::{Logical, Point, Rectangle, Scale, Size},
};

use crate::{state::WithState, window::WindowElement};

/// Border width in logical pixels.
pub const BORDER_WIDTH: i32 = 2;

/// Colors for window borders.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BorderColors {
    pub focused: [f32; 4],
    pub unfocused: [f32; 4],
}

impl Default for BorderColors {
    /// Neutral grays, until the wallpaper's colors are known.
    fn default() -> Self {
        Self {
            focused: [0.75, 0.75, 0.75, 1.0],
            unfocused: [0.3, 0.3, 0.3, 1.0],
        }
    }
}

/// The four edge buffers of one window's border.
///
/// The buffers keep stable ids so unchanged borders aren't redrawn every frame.
#[derive(Debug)]
pub struct Border {
    edges: [SolidColorBuffer; 4],
}

impl Default for Border {
    fn default() -> Self {
        Self {
            edges: std::array::from_fn(|_| SolidColorBuffer::default()),
        }
    }
}

impl Border {
    /// Render elements for a border around `rect` (output-relative, logical).
    pub fn render_elements(
        &mut self,
        rect: Rectangle<f64, Logical>,
        color: [f32; 4],
        alpha: f32,
        scale: Scale<f64>,
    ) -> Vec<SolidColorRenderElement> {
        let w = BORDER_WIDTH as f64;
        let Rectangle { loc, size } = rect;

        let sides: [(Point<f64, Logical>, Size<f64, Logical>); 4] = [
            // top, bottom, left, right
            ((loc.x - w, loc.y - w).into(), (size.w + 2.0 * w, w).into()),
            ((loc.x - w, loc.y + size.h).into(), (size.w + 2.0 * w, w).into()),
            ((loc.x - w, loc.y).into(), (w, size.h).into()),
            ((loc.x + size.w, loc.y).into(), (w, size.h).into()),
        ];

        self.edges
            .iter_mut()
            .zip(sides)
            .map(|(buffer, (side_loc, side_size))| {
                buffer.update(side_size.to_i32_round::<i32>(), color);
                SolidColorRenderElement::from_buffer(
                    buffer,
                    side_loc.to_physical_precise_round(scale),
                    scale,
                    alpha,
                    Kind::Unspecified,
                )
            })
            .collect()
    }
}

impl WindowElement {
    /// Whether this window is the activated (keyboard-focused) one.
    pub fn is_activated(&self) -> bool {
        match self.underlying_surface() {
            // Pending, so the border changes as soon as focus does.
            WindowSurface::Wayland(toplevel) => toplevel
                .with_pending_state(|state| state.states.contains(xdg_toplevel::State::Activated)),
            WindowSurface::X11(surface) => surface.is_activated(),
        }
    }

    /// Whether this window should get a compositor-drawn border.
    pub fn wants_border(&self) -> bool {
        !self.is_x11_override_redirect()
            && self.with_state(|state| !state.layout_mode.is_fullscreen())
    }
}
