// SPDX-License-Identifier: GPL-3.0-or-later

//! Window borders, drawn by the compositor.

#[cfg(feature = "testing")]
use smithay::backend::renderer::test::DummyRenderer;
use smithay::{
    backend::renderer::{
        RendererSuper,
        element::{self, Element, Kind, RenderElement},
        gles::{
            GlesPixelProgram, GlesRenderer, Uniform, UniformName, UniformType,
            element::PixelShaderElement,
        },
        utils::{CommitCounter, OpaqueRegions},
    },
    desktop::WindowSurface,
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel,
    utils::{Buffer, Logical, Physical, Rectangle, Scale},
};

use crate::{backend::udev::UdevRenderer, state::WithState, window::WindowElement};

/// Border width in logical pixels.
pub const BORDER_WIDTH: i32 = 2;

/// How round windows are, in logical pixels.
pub const CORNER_RADIUS: i32 = 10;

/// How windows look.
#[derive(Debug, Clone, Copy)]
pub struct WindowStyle {
    /// The radius of their corners in logical pixels.
    pub radius: i32,
    /// Whether the wallpaper shows through them blurred.
    pub blur: bool,
}

impl Default for WindowStyle {
    fn default() -> Self {
        Self {
            radius: CORNER_RADIUS,
            blur: true,
        }
    }
}

/// Colors for window borders.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BorderColors {
    pub focused: [f32; 4],
    pub unfocused: [f32; 4],
}

impl Default for BorderColors {
    /// Material's purple, for until the wallpaper's colors are known.
    fn default() -> Self {
        Self {
            focused: [0.816, 0.737, 1.0, 1.0],
            unfocused: [0.286, 0.271, 0.31, 1.0],
        }
    }
}

const BORDER_SHADER: &str = r#"
precision highp float;

uniform float alpha;
uniform vec2 size;
varying vec2 v_coords;
uniform float radius;
uniform float width;
uniform vec4 color;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

float rounded(vec2 position, vec2 box, float corner_radius) {
    vec2 corner = abs(position - box * 0.5) - box * 0.5 + corner_radius;
    return length(max(corner, 0.0)) + min(max(corner.x, corner.y), 0.0) - corner_radius;
}

void main() {
    vec2 position = v_coords * size;
    float outer = rounded(position, size, radius);
    float inner = rounded(position - width, size - width * 2.0, max(radius - width, 0.0));

    float coverage = clamp(0.5 - outer, 0.0, 1.0) * clamp(0.5 + inner, 0.0, 1.0);
    gl_FragColor = vec4(color.rgb, 1.0) * color.a * coverage * alpha;
}
"#;

/// The shader, compiled once per GPU context.
#[derive(Clone)]
struct Program(GlesPixelProgram);

fn program(renderer: &mut GlesRenderer) -> Option<GlesPixelProgram> {
    if let Some(program) = renderer.egl_context().user_data().get::<Program>() {
        return Some(program.0.clone());
    }

    let program = renderer
        .compile_custom_pixel_shader(
            BORDER_SHADER,
            &[
                UniformName::new("radius", UniformType::_1f),
                UniformName::new("width", UniformType::_1f),
                UniformName::new("color", UniformType::_4f),
            ],
        )
        .map_err(|err| tracing::warn!("Failed to compile the border shader: {err}"))
        .ok()?;

    renderer
        .egl_context()
        .user_data()
        .insert_if_missing(|| Program(program.clone()));
    Some(program)
}

/// One window's border.
///
/// This keeps its element, whose id has to stay the same for it not to be redrawn every frame.
#[derive(Debug, Default)]
pub struct Border {
    element: Option<PixelShaderElement>,
    /// What the element was last updated with.
    drawn: Option<(Rectangle<i32, Logical>, [f32; 4], i32, f64)>,
}

impl Border {
    /// The border around `rect` (output-relative, logical), with `radius` being the
    /// radius of the window's corners.
    pub fn render_element(
        &mut self,
        renderer: &mut GlesRenderer,
        rect: Rectangle<f64, Logical>,
        color: [f32; 4],
        alpha: f32,
        radius: i32,
        scale: Scale<f64>,
    ) -> Option<BorderElement> {
        let width = BORDER_WIDTH as f64;
        let area = Rectangle::<f64, Logical>::new(
            (rect.loc.x - width, rect.loc.y - width).into(),
            (rect.size.w + width * 2.0, rect.size.h + width * 2.0).into(),
        )
        .to_i32_round();

        let color = [color[0], color[1], color[2], color[3] * alpha];
        let drawn = Some((area, color, radius, scale.x));

        let element = match self.element.as_mut() {
            Some(element) => element,
            None => self.element.insert(PixelShaderElement::new(
                program(renderer)?,
                area,
                None,
                1.0,
                Vec::new(),
                Kind::Unspecified,
            )),
        };

        if self.drawn != drawn {
            self.drawn = drawn;
            element.resize(area, None);
            element.update_uniforms(vec![
                Uniform::new("radius", ((radius + BORDER_WIDTH) as f64 * scale.x) as f32),
                Uniform::new("width", (width * scale.x) as f32),
                Uniform::new("color", (color[0], color[1], color[2], color[3])),
            ]);
        }

        Some(BorderElement(element.clone()))
    }
}

/// A window's border, as an element for any of the renderers.
#[derive(Debug)]
pub struct BorderElement(PixelShaderElement);

impl Element for BorderElement {
    fn id(&self) -> &element::Id {
        self.0.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.0.current_commit()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.0.src()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.0.geometry(scale)
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        self.0.opaque_regions(scale)
    }

    fn alpha(&self) -> f32 {
        self.0.alpha()
    }

    fn kind(&self) -> Kind {
        self.0.kind()
    }
}

impl RenderElement<GlesRenderer> for BorderElement {
    fn draw(
        &self,
        frame: &mut <GlesRenderer as RendererSuper>::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), <GlesRenderer as RendererSuper>::Error> {
        self.0.draw(frame, src, dst, damage, opaque_regions)
    }
}

impl<'a> RenderElement<UdevRenderer<'a>> for BorderElement {
    fn draw(
        &self,
        frame: &mut <UdevRenderer<'a> as RendererSuper>::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), <UdevRenderer<'a> as RendererSuper>::Error> {
        RenderElement::<GlesRenderer>::draw(
            &self.0,
            frame.as_mut(),
            src,
            dst,
            damage,
            opaque_regions,
        )?;
        Ok(())
    }
}

#[cfg(feature = "testing")]
impl RenderElement<DummyRenderer> for BorderElement {
    fn draw(
        &self,
        _frame: &mut <DummyRenderer as RendererSuper>::Frame<'static, 'static>,
        _src: Rectangle<f64, Buffer>,
        _dst: Rectangle<i32, Physical>,
        _damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), <DummyRenderer as RendererSuper>::Error> {
        Ok(())
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
