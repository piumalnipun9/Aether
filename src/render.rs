// SPDX-License-Identifier: GPL-3.0-or-later

pub mod border;
pub mod pointer;
pub mod render_elements;
pub mod rounded;
pub mod texture;
pub mod util;

use itertools::Itertools;
use smithay::{
    backend::renderer::{
        ImportAll, ImportMem, Renderer, RendererSuper, Texture,
        element::{
            self, AsRenderElements, Element, RenderElementStates,
            solid::SolidColorRenderElement,
            surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
            utils::RescaleRenderElement,
        },
        gles::GlesRenderer,
        utils::CommitCounter,
    },
    desktop::{
        PopupManager, Space, WindowSurface, layer_map_for_output,
        utils::{
            OutputPresentationFeedback, surface_presentation_feedback_flags_from_states,
            surface_primary_scanout_output,
        },
    },
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Physical, Point, Rectangle, Scale},
    wayland::shell::wlr_layer,
};
use util::{snapshot::SnapshotRenderElement, surface::WlSurfaceTextureRenderElement};

use crate::{
    backend::{Backend, udev::UdevRenderer},
    decoration::DecorationSurface,
    pinnacle_render_elements,
    state::{State, WithState},
    window::{WindowElement, ZIndexElement},
};

use self::{
    pointer::PointerRenderElement,
    rounded::{RoundedElement, Rounding},
    util::surface::texture_render_elements_from_surface_tree,
};

pub const CLEAR_COLOR: [f32; 4] = [0.078, 0.071, 0.094, 1.0];
pub const CLEAR_COLOR_LOCKED: [f32; 4] = [0.2, 0.0, 0.3, 1.0];

pinnacle_render_elements! {
    #[derive(Debug)]
    pub enum OutputRenderElement<R> {
        Surface = WaylandSurfaceRenderElement<R>,
        Pointer = PointerRenderElement<R>,
        Snapshot = SnapshotRenderElement,
        SolidColor = SolidColorRenderElement,
        /// A window surface scaled by its open animation.
        RescaledSurface = RescaleRenderElement<WaylandSurfaceRenderElement<R>>,
        /// A window snapshot scaled by its close animation.
        RescaledSnapshot = RescaleRenderElement<SnapshotRenderElement>,
        /// A shell window.
        Shell = element::memory::MemoryRenderBufferRenderElement<R>,
        /// The blurred scene behind quick settings.
        Backdrop = texture::CommonTextureRenderElement,
        /// A window's border.
        Border = border::BorderElement,
        /// A window surface with rounded corners.
        RoundedSurface = RoundedElement<WaylandSurfaceRenderElement<R>>,
        /// The same, scaled by an animation.
        RescaledRoundedSurface = RescaleRenderElement<RoundedElement<WaylandSurfaceRenderElement<R>>>,
        /// A window snapshot with rounded corners.
        RoundedSnapshot = RoundedElement<SnapshotRenderElement>,
        /// The same, scaled by an animation.
        RescaledRoundedSnapshot = RescaleRenderElement<RoundedElement<SnapshotRenderElement>>,
        /// The blurred wallpaper behind a window.
        WindowBackdrop = RoundedElement<texture::CommonTextureRenderElement>,
        /// The same, scaled by an animation.
        RescaledWindowBackdrop = RescaleRenderElement<RoundedElement<texture::CommonTextureRenderElement>>,
    }
}

/// Trait to reduce bound specifications.
pub trait PRenderer
where
    Self: Renderer<TextureId = Self::PTextureId, Error = Self::PError> + ImportAll + ImportMem,
    <Self as RendererSuper>::TextureId: Texture + Clone + 'static,
{
    // Self::TextureId: Texture + Clone + 'static doesn't work in the where clause,
    // which is why these associated types exist.
    //
    // From https://github.com/YaLTeR/niri/blob/ae7fb4c4f405aa0ff49930040d414581a812d938/src/render_helpers/renderer.rs#L10
    type PTextureId: Texture + Clone + Send + 'static;
    type PError: std::error::Error + Send + Sync + 'static;
}

impl<R> PRenderer for R
where
    R: ImportAll + ImportMem,
    R::TextureId: Texture + Clone + Send + 'static,
    R::Error: std::error::Error + Send + Sync + 'static,
{
    type PTextureId = R::TextureId;
    type PError = R::Error;
}

/// Trait for renderers that provide [`GlesRenderer`]s.
pub trait AsGlesRenderer {
    /// Gets a [`GlesRenderer`] from this renderer.
    fn as_gles_renderer(&mut self) -> &mut GlesRenderer;
}

impl AsGlesRenderer for GlesRenderer {
    fn as_gles_renderer(&mut self) -> &mut GlesRenderer {
        self
    }
}

impl AsGlesRenderer for UdevRenderer<'_> {
    fn as_gles_renderer(&mut self) -> &mut GlesRenderer {
        self.as_mut()
    }
}

#[derive(Debug)]
pub struct SplitRenderElements<E> {
    pub surface_elements: Vec<E>,
    pub popup_elements: Vec<E>,
}

impl<E> Default for SplitRenderElements<E> {
    fn default() -> Self {
        Self {
            surface_elements: Default::default(),
            popup_elements: Default::default(),
        }
    }
}

// Renders popup elements for the given toplevel surface.
fn popup_render_elements<R: PRenderer>(
    surface: &WlSurface,
    renderer: &mut R,
    location: Point<i32, Physical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Vec<WaylandSurfaceRenderElement<R>> {
    let popup_elements = PopupManager::popups_for_surface(surface)
        .flat_map(|(popup, popup_offset)| {
            let offset = (popup_offset - popup.geometry().loc).to_physical_precise_round(scale);

            render_elements_from_surface_tree(
                renderer,
                popup.wl_surface(),
                location + offset,
                scale,
                alpha,
                element::Kind::Unspecified,
            )
        })
        .collect();

    popup_elements
}

impl WindowElement {
    /// Renders surface and popup elements for this window at the given *logical* location in the space,
    /// output-relative.
    pub fn render_elements<R: PRenderer>(
        &self,
        renderer: &mut R,
        location: Point<i32, Logical>,
        scale: Scale<f64>,
        alpha: f32,
        include_decorations: bool,
    ) -> SplitRenderElements<WaylandSurfaceRenderElement<R>> {
        let _span = tracy_client::span!("WindowElement::render_elements");

        let total_deco_offset = if include_decorations {
            self.total_decoration_offset()
        } else {
            Default::default()
        };

        let window_location = if include_decorations {
            self.geometry().loc
        } else {
            self.geometry_without_decorations().loc
        };

        let window_location = (location - window_location).to_physical_precise_round(scale);

        // Popups render relative to the actual window, so offset by the decoration offset.
        let surface_location = (location + total_deco_offset).to_physical_precise_round(scale);

        let (deco_elems_under, deco_elems_over) =
            if self.should_not_have_ssd() || !include_decorations {
                (Vec::new(), Vec::new())
            } else {
                self.with_state(|state| {
                    let mut surfaces = state.decoration_surfaces.iter().collect::<Vec<_>>();
                    surfaces.sort_by_key(|deco| deco.z_index());
                    let mut surfaces = surfaces.into_iter().rev().peekable();

                    let mut deco_to_elems = |deco: &DecorationSurface| {
                        let deco_location = {
                            let deco_loc =
                                location + deco.location() + total_deco_offset - deco.offset();
                            deco_loc.to_physical_precise_round(scale)
                        };

                        let surface_elements: Vec<WaylandSurfaceRenderElement<_>> =
                            render_elements_from_surface_tree(
                                renderer,
                                deco.wl_surface(),
                                deco_location,
                                scale,
                                alpha,
                                element::Kind::Unspecified,
                            );
                        surface_elements
                    };

                    let deco_elems_over = surfaces
                        .peeking_take_while(|deco| deco.z_index() >= 0)
                        .flat_map(&mut deco_to_elems)
                        .collect::<Vec<_>>();

                    let deco_elems_under = surfaces.flat_map(deco_to_elems).collect::<Vec<_>>();

                    (deco_elems_under, deco_elems_over)
                })
            };

        match self.underlying_surface() {
            WindowSurface::Wayland(toplevel) => {
                let surface = toplevel.wl_surface();

                let surface_elements = deco_elems_over
                    .into_iter()
                    .chain(render_elements_from_surface_tree(
                        renderer,
                        surface,
                        window_location,
                        scale,
                        alpha,
                        element::Kind::Unspecified,
                    ))
                    .chain(deco_elems_under)
                    .collect::<Vec<_>>();

                let popup_elements =
                    popup_render_elements(surface, renderer, surface_location, scale, alpha);

                SplitRenderElements {
                    surface_elements,
                    popup_elements,
                }
            }
            WindowSurface::X11(s) => {
                let surface_elements = deco_elems_over
                    .into_iter()
                    .chain(AsRenderElements::render_elements(
                        s,
                        renderer,
                        window_location,
                        scale,
                        alpha,
                    ))
                    .chain(deco_elems_under)
                    .collect();
                SplitRenderElements {
                    surface_elements,
                    popup_elements: Vec::new(),
                }
            }
        }
    }

    /// Render elements for this window as textures.
    pub fn texture_render_elements<R: PRenderer + AsGlesRenderer>(
        &self,
        renderer: &mut R,
        location: Point<i32, Logical>,
        scale: Scale<f64>,
        alpha: f32,
    ) -> SplitRenderElements<WlSurfaceTextureRenderElement> {
        let _span = tracy_client::span!("WindowElement::texture_render_elements");

        let popup_location = location.to_physical_precise_round(scale);
        let window_location = (location - self.geometry().loc).to_physical_precise_round(scale);

        let (deco_elems_under, deco_elems_over) = if self.should_not_have_ssd() {
            (Vec::new(), Vec::new())
        } else {
            let total_deco_offset = self.total_decoration_offset();
            self.with_state(|state| {
                let mut surfaces = state.decoration_surfaces.iter().collect::<Vec<_>>();
                surfaces.sort_by_key(|deco| deco.z_index());
                let mut surfaces = surfaces.into_iter().rev().peekable();

                let mut deco_to_elems = |deco: &DecorationSurface| {
                    let deco_location = {
                        let deco_loc =
                            location + deco.location() + total_deco_offset - deco.offset();
                        deco_loc.to_physical_precise_round(scale)
                    };

                    let surface_elements = texture_render_elements_from_surface_tree(
                        renderer.as_gles_renderer(),
                        deco.wl_surface(),
                        deco_location,
                        scale,
                        alpha,
                    );
                    surface_elements
                };

                let deco_elems_over = surfaces
                    .peeking_take_while(|deco| deco.z_index() >= 0)
                    .flat_map(&mut deco_to_elems)
                    .collect::<Vec<_>>();

                let deco_elems_under = surfaces.flat_map(deco_to_elems).collect::<Vec<_>>();

                (deco_elems_under, deco_elems_over)
            })
        };

        match self.underlying_surface() {
            WindowSurface::Wayland(toplevel) => {
                let surface = toplevel.wl_surface();

                let surface_elements = deco_elems_over
                    .into_iter()
                    .chain(texture_render_elements_from_surface_tree(
                        renderer.as_gles_renderer(),
                        surface,
                        window_location,
                        scale,
                        alpha,
                    ))
                    .chain(deco_elems_under)
                    .collect::<Vec<_>>();

                let popup_elements = PopupManager::popups_for_surface(surface)
                    .flat_map(|(popup, popup_offset)| {
                        let offset = (self.geometry().loc + popup_offset - popup.geometry().loc)
                            .to_physical_precise_round(scale);

                        texture_render_elements_from_surface_tree(
                            renderer.as_gles_renderer(),
                            popup.wl_surface(),
                            popup_location + offset,
                            scale,
                            alpha,
                        )
                    })
                    .collect();

                SplitRenderElements {
                    surface_elements,
                    popup_elements,
                }
            }
            WindowSurface::X11(surface) => {
                let surface_elements = if let Some(surface) = surface.wl_surface() {
                    texture_render_elements_from_surface_tree(
                        renderer.as_gles_renderer(),
                        &surface,
                        window_location,
                        scale,
                        alpha,
                    )
                } else {
                    Vec::new()
                };

                let surface_elements = deco_elems_over
                    .into_iter()
                    .chain(surface_elements)
                    .chain(deco_elems_under)
                    .collect();
                SplitRenderElements {
                    surface_elements,
                    popup_elements: Vec::new(),
                }
            }
        }
    }
}

struct LayerRenderElements<R: PRenderer> {
    popup: Vec<WaylandSurfaceRenderElement<R>>,
    background: Vec<WaylandSurfaceRenderElement<R>>,
    bottom: Vec<WaylandSurfaceRenderElement<R>>,
    top: Vec<WaylandSurfaceRenderElement<R>>,
    overlay: Vec<WaylandSurfaceRenderElement<R>>,
}

fn layer_render_elements<R: PRenderer>(
    output: &Output,
    renderer: &mut R,
    scale: Scale<f64>,
) -> LayerRenderElements<R> {
    let _span = tracy_client::span!("layer_render_elements");

    let layer_map = layer_map_for_output(output);
    let mut popup = Vec::new();
    let mut overlay = Vec::new();
    let mut top = Vec::new();
    let mut bottom = Vec::new();
    let mut background = Vec::new();

    let layer_elements = layer_map
        .layers()
        .rev()
        .filter_map(|surface| {
            layer_map
                .layer_geometry(surface)
                .map(|geo| (surface, geo.loc))
        })
        .map(|(surface, loc)| {
            let loc = loc.to_physical_precise_round(scale);
            let surface_elements = render_elements_from_surface_tree(
                renderer,
                surface.wl_surface(),
                loc,
                scale,
                1.0,
                element::Kind::Unspecified,
            );
            let popup_elements =
                popup_render_elements(surface.wl_surface(), renderer, loc, scale, 1.0);

            let elements = SplitRenderElements {
                surface_elements,
                popup_elements,
            };

            (surface.layer(), elements)
        });

    for (layer, elements) in layer_elements {
        let SplitRenderElements {
            surface_elements,
            popup_elements,
        } = elements;

        popup.extend(popup_elements);
        match layer {
            wlr_layer::Layer::Background => background.extend(surface_elements),
            wlr_layer::Layer::Bottom => bottom.extend(surface_elements),
            wlr_layer::Layer::Top => top.extend(surface_elements),
            wlr_layer::Layer::Overlay => overlay.extend(surface_elements),
        }
    }

    LayerRenderElements {
        popup,
        background,
        bottom,
        top,
        overlay,
    }
}

struct WindowRenderElements<R: PRenderer> {
    popups: Vec<OutputRenderElement<R>>,
    fullscreen_and_up: Vec<OutputRenderElement<R>>,
    rest: Vec<OutputRenderElement<R>>,
}

/// Renders surface and popup elements for windows on active workspaces.
fn window_render_elements<R: PRenderer + AsGlesRenderer>(
    output: &Output,
    space: &Space<WindowElement>,
    renderer: &mut R,
    scale: Scale<f64>,
    z_index_stack: &[ZIndexElement],
) -> WindowRenderElements<R> {
    let _span = tracy_client::span!("window_render_elements");

    let windows = space.elements_for_output(output);

    let mut last_fullscreen_split_at = 0;

    let mut renderables = Vec::new();

    let mut z_index_elements = z_index_stack.iter();

    for window in windows {
        let mut unmapping_windows = Vec::new();
        let found = z_index_elements.any(|elem| match elem {
            ZIndexElement::Window(win) => win == window,
            ZIndexElement::Unmapping(weak) => {
                if let Some(strong) = weak.upgrade() {
                    unmapping_windows.push(strong);
                }
                false
            }
        });
        assert!(found);
        renderables.extend(unmapping_windows.into_iter().map(itertools::Either::Right));
        renderables.push(itertools::Either::Left(window));
    }

    renderables.extend(z_index_elements.filter_map(|z| match z {
        ZIndexElement::Window(_) => None,
        ZIndexElement::Unmapping(weak) => weak.upgrade().map(itertools::Either::Right),
    }));

    let mut popups = Vec::new();

    // While switching workspaces, the new workspace slides in and the old one slides out.
    let output_width = space
        .output_geometry(output)
        .map(|geo| geo.size.w)
        .unwrap_or_default();
    let (old_ws_offset, new_ws_offset, switching) =
        output.with_state(|state| match state.workspace_switch.as_ref() {
            Some(switch) => {
                let (old, new) = switch.offsets(output_width);
                (old, new, true)
            }
            None => (0.0, 0.0, false),
        });
    let new_ws_offset = Point::<f64, Logical>::from((new_ws_offset, 0.0));

    let style = output.with_state(|state| state.window_style);
    let output_rect = space
        .output_geometry(output)
        .map(|geo| Rectangle::from_size(geo.size.to_physical_precise_round(scale)))
        .unwrap_or_default();
    let blurred = style
        .blur
        .then(|| crate::shell::BlurredWallpaper::for_output(output, space, renderer))
        .flatten();

    let mut fullscreen_and_up = renderables
        .into_iter()
        .rev()
        .enumerate()
        .map(|(i, win)| match win {
            itertools::Either::Left(win) => {
                if win.with_state(|state| state.layout_mode.is_fullscreen()) {
                    last_fullscreen_split_at = i + 1;
                }

                let loc = (space.element_location(win).unwrap_or_default()
                    - output.current_location())
                .to_f64()
                    + win.move_offset()
                    + new_ws_offset;
                let loc_f = loc;
                let loc = loc.to_i32_round();

                let appearance = win.open_appearance();
                let fullscreen = win.with_state(|state| state.layout_mode.is_fullscreen());
                let radius = if fullscreen { 0 } else { style.radius };
                let window_rect = Rectangle::<i32, Physical>::new(
                    loc.to_physical_precise_round(scale),
                    win.geometry().size.to_physical_precise_round(scale),
                );
                let rounding = (radius > 0)
                    .then(|| {
                        Rounding::new(
                            renderer.as_gles_renderer(),
                            window_rect,
                            radius as f32 * scale.x as f32,
                        )
                    })
                    .flatten();

                let SplitRenderElements {
                    surface_elements,
                    popup_elements,
                } = win.render_elements(renderer, loc, scale, appearance.alpha, true);

                popups.extend(popup_elements.into_iter().map(OutputRenderElement::from));

                // Opening windows scale about their center; resizing ones stretch from
                // their top-left. A window that opens and resizes at once only does the former.
                let (origin, elem_scale) = if !appearance.is_identity() {
                    let center = loc + win.geometry().size.downscale(2);
                    (center, Some(Scale::from(appearance.scale)))
                } else {
                    let (sx, sy) = win.move_scale();
                    let stretched = (sx - 1.0).abs() > 0.001 || (sy - 1.0).abs() > 0.001;
                    (loc, stretched.then(|| Scale::from((sx, sy))))
                };

                // The border follows the window as drawn, under it.
                let border_elements = if win.wants_border() {
                    let size = win.geometry().size.to_f64();
                    let rect = if !appearance.is_identity() {
                        let s = appearance.scale;
                        let scaled = size.upscale(s);
                        let center = loc_f + size.downscale(2.0).to_point();
                        Rectangle::new(center - scaled.downscale(2.0).to_point(), scaled)
                    } else {
                        let (sx, sy) = win.move_scale();
                        Rectangle::new(loc_f, (size.w * sx, size.h * sy).into())
                    };
                    let colors = output.with_state(|state| state.border_colors);
                    let color = if win.is_activated() {
                        colors.focused
                    } else {
                        colors.unfocused
                    };
                    win.with_state_mut(|state| {
                        state.border.render_element(
                            renderer.as_gles_renderer(),
                            rect,
                            color,
                            appearance.border_alpha,
                            radius,
                            scale,
                        )
                    })
                } else {
                    None
                };
                let border_elements = border_elements.into_iter().map(OutputRenderElement::from);

                // The wallpaper shows through windows that let it, as long as nothing
                // covers them entirely.
                let backdrop_element = blurred.as_ref().and_then(|blurred| {
                    if fullscreen || win.is_x11_override_redirect() {
                        return None;
                    }
                    blurred.element(
                        renderer.as_gles_renderer(),
                        win,
                        window_rect,
                        output_rect,
                        appearance.alpha,
                        scale,
                    )
                });
                let rounding_for_backdrop = (radius > 0).then(|| rounding.clone()).flatten();

                let iter: Box<dyn Iterator<Item = OutputRenderElement<R>>> =
                    match (elem_scale, rounding) {
                        (None, None) => {
                            Box::new(surface_elements.into_iter().map(OutputRenderElement::from))
                        }
                        (None, Some(rounding)) => {
                            Box::new(surface_elements.into_iter().map(move |elem| {
                                OutputRenderElement::from(rounding.surface(elem, scale))
                            }))
                        }
                        (Some(elem_scale), None) => {
                            let origin = origin.to_physical_precise_round(scale);
                            Box::new(surface_elements.into_iter().map(move |elem| {
                                OutputRenderElement::from(RescaleRenderElement::from_element(
                                    elem, origin, elem_scale,
                                ))
                            }))
                        }
                        (Some(elem_scale), Some(rounding)) => {
                            let origin = origin.to_physical_precise_round(scale);
                            Box::new(surface_elements.into_iter().map(move |elem| {
                                OutputRenderElement::from(RescaleRenderElement::from_element(
                                    rounding.surface(elem, scale),
                                    origin,
                                    elem_scale,
                                ))
                            }))
                        }
                    };

                let backdrop =
                    backdrop_element.and_then(|(element, texture_size, src, geometry)| {
                        let rounding = match rounding_for_backdrop {
                            Some(rounding) => rounding,
                            None => Rounding::new(renderer.as_gles_renderer(), window_rect, 0.0)?,
                        };
                        let element = rounding.region(element, texture_size, src, geometry);
                        Some(match elem_scale {
                            None => OutputRenderElement::from(element),
                            Some(elem_scale) => {
                                OutputRenderElement::from(RescaleRenderElement::from_element(
                                    element,
                                    origin.to_physical_precise_round(scale),
                                    elem_scale,
                                ))
                            }
                        })
                    });

                Box::new(iter.chain(border_elements).chain(backdrop)) as Box<dyn Iterator<Item = _>>
            }
            itertools::Either::Right(snap) => {
                // The old workspace's windows are drawn from the switch's own snapshots.
                if switching && snap.close_animation.is_none() {
                    return Box::new(std::iter::empty()) as Box<dyn Iterator<Item = _>>;
                }
                if snap.fullscreen {
                    last_fullscreen_split_at = i + 1;
                }
                let appearance = snap.appearance();
                let loc = (snap.space_loc - output.current_location()).to_f64() + new_ws_offset;
                let loc = loc.to_physical_precise_round(scale);
                let elem = snap
                    .snapshot
                    .render_elements(renderer, loc, scale, appearance.alpha);
                let rounding = (style.radius > 0 && !snap.fullscreen)
                    .then(|| {
                        Rounding::new(
                            renderer.as_gles_renderer(),
                            Rectangle::default(),
                            style.radius as f32 * scale.x as f32,
                        )
                    })
                    .flatten();
                let iter: Box<dyn Iterator<Item = OutputRenderElement<R>>> =
                    match (appearance.is_identity(), rounding) {
                        (true, None) => Box::new(elem.into_iter().map(OutputRenderElement::from)),
                        (true, Some(rounding)) => Box::new(elem.into_iter().map(move |elem| {
                            OutputRenderElement::from(rounding.whole(elem, scale))
                        })),
                        (false, rounding) => Box::new(elem.into_iter().map(move |elem| {
                            let geo = elem.geometry(scale);
                            let origin = geo.loc + geo.size.downscale(2).to_point();
                            match &rounding {
                                Some(rounding) => {
                                    OutputRenderElement::from(RescaleRenderElement::from_element(
                                        rounding.whole(elem, scale),
                                        origin,
                                        appearance.scale,
                                    ))
                                }
                                None => {
                                    OutputRenderElement::from(RescaleRenderElement::from_element(
                                        elem,
                                        origin,
                                        appearance.scale,
                                    ))
                                }
                            }
                        })),
                    };
                iter
            }
        })
        .collect::<Vec<_>>();

    // The old workspace slides out underneath everything else.
    if switching {
        let old_elements = output.with_state(|state| {
            let Some(switch) = state.workspace_switch.as_ref() else {
                return Vec::new();
            };
            switch
                .old_windows
                .iter()
                .rev()
                .filter_map(|(snapshot, loc)| {
                    let loc = (loc.to_f64() + Point::from((old_ws_offset, 0.0)))
                        .to_physical_precise_round(scale);
                    let element = snapshot.render_elements(renderer, loc, scale, 1.0)?;
                    let rounding = (style.radius > 0)
                        .then(|| {
                            Rounding::new(
                                renderer.as_gles_renderer(),
                                Rectangle::default(),
                                style.radius as f32 * scale.x as f32,
                            )
                        })
                        .flatten();
                    Some(match rounding {
                        Some(rounding) => OutputRenderElement::from(rounding.whole(element, scale)),
                        None => OutputRenderElement::from(element),
                    })
                })
                .collect::<Vec<_>>()
        });
        fullscreen_and_up.push(Box::new(old_elements.into_iter()));
    }

    let rest = fullscreen_and_up.split_off(last_fullscreen_split_at);

    WindowRenderElements {
        popups,
        fullscreen_and_up: fullscreen_and_up.into_iter().flatten().collect(),
        rest: rest.into_iter().flatten().collect(),
    }
}

/// Renders elements for the given output.
pub fn output_render_elements<R: PRenderer + AsGlesRenderer>(
    output: &Output,
    renderer: &mut R,
    space: &Space<WindowElement>,
    z_index_stack: &[ZIndexElement],
) -> Vec<OutputRenderElement<R>> {
    let _span = tracy_client::span!("output_render_elements");

    let scale = Scale::from(output.current_scale().fractional_scale());

    let mut output_render_elements: Vec<OutputRenderElement<_>> = Vec::new();

    let LayerRenderElements {
        popup: layer_popups,
        background,
        bottom,
        top,
        overlay,
    } = layer_render_elements(output, renderer, scale);

    let WindowRenderElements {
        popups: window_popups,
        fullscreen_and_up: fullscreen_and_up_elements,
        rest: rest_of_window_elements,
    } = window_render_elements::<R>(output, space, renderer, scale, z_index_stack);

    // Elements render from top to bottom

    // Things that change how windows look don't count as damage on their own.
    if output.with_state_mut(|state| {
        let redraw = state.full_redraws > 0;
        state.full_redraws = state.full_redraws.saturating_sub(1);
        redraw
    }) && let Some(geo) = space.output_geometry(output)
    {
        output_render_elements.push(OutputRenderElement::from(SolidColorRenderElement::new(
            element::Id::new(),
            Rectangle::from_size(geo.size.to_physical_precise_round(scale)),
            CommitCounter::default(),
            [0.0, 0.0, 0.0, 0.0],
            element::Kind::Unspecified,
        )));
    }

    output_render_elements.extend(crate::shell::panel_render_elements(output, renderer, space));
    output_render_elements.extend(layer_popups.into_iter().map(OutputRenderElement::from));
    output_render_elements.extend(window_popups);
    output_render_elements.extend(overlay.into_iter().map(OutputRenderElement::from));
    output_render_elements.extend(fullscreen_and_up_elements);
    output_render_elements.extend(
        output
            .with_state(|state| state.bar.as_ref()?.render_element(renderer))
            .map(OutputRenderElement::from),
    );
    output_render_elements.extend(top.into_iter().map(OutputRenderElement::from));
    output_render_elements.extend(rest_of_window_elements);
    output_render_elements.extend(bottom.into_iter().map(OutputRenderElement::from));
    output_render_elements.extend(background.into_iter().map(OutputRenderElement::from));
    output_render_elements.extend(
        output
            .with_state(|state| state.wallpaper.as_ref()?.render_element(renderer))
            .map(OutputRenderElement::from),
    );

    output_render_elements
}

// TODO: docs
pub fn take_presentation_feedback(
    output: &Output,
    space: &Space<WindowElement>,
    render_element_states: &RenderElementStates,
) -> OutputPresentationFeedback {
    let _span = tracy_client::span!("take_presentation_feedback");

    let mut output_presentation_feedback = OutputPresentationFeedback::new(output);

    space.elements().for_each(|window| {
        if space.outputs_for_element(window).contains(output) {
            window.take_presentation_feedback(
                &mut output_presentation_feedback,
                surface_primary_scanout_output,
                |surface, _| {
                    surface_presentation_feedback_flags_from_states(surface, render_element_states)
                },
            );
        }
    });

    let map = smithay::desktop::layer_map_for_output(output);
    for layer_surface in map.layers() {
        layer_surface.take_presentation_feedback(
            &mut output_presentation_feedback,
            surface_primary_scanout_output,
            |surface, _| {
                surface_presentation_feedback_flags_from_states(surface, render_element_states)
            },
        );
    }

    output_presentation_feedback
}

impl State {
    /// Schedule a new render.
    pub fn schedule_render(&mut self, output: &Output) {
        let _span = tracy_client::span!("State::schedule_render");

        match &mut self.backend {
            Backend::Udev(udev) => {
                udev.schedule_render(output);
            }
            Backend::Winit(winit) => {
                winit.schedule_render();
            }
            #[cfg(feature = "testing")]
            Backend::Dummy(_) => (),
        }
    }
}
