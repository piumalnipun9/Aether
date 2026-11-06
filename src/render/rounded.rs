// SPDX-License-Identifier: GPL-3.0-or-later

//! Cutting windows to rounded rectangles.
//!
//! A window is made of any number of surfaces, each with a texture of its own, so what's
//! cut off of each depends on where it is in the window. The textures are drawn as usual, just
//! with a shader that leaves out what's outside of the window's rounded rectangle.

#[cfg(feature = "testing")]
use smithay::backend::renderer::test::DummyRenderer;
use smithay::{
    backend::renderer::{
        RendererSuper,
        element::{self, Element, RenderElement, surface::WaylandSurfaceRenderElement},
        gles::{GlesRenderer, GlesTexProgram, Uniform, UniformName, UniformType},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    utils::{Buffer, Physical, Point, Rectangle, Scale, Size, Transform},
};

use crate::backend::udev::UdevRenderer;

const SHADER: &str = r#"#version 100
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision highp float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;
// What turns texture coordinates into pixels from the window's corner.
uniform vec2 pixels_scale;
uniform vec2 pixels_offset;
// The window's size in pixels.
uniform vec2 size;
uniform float radius;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

void main() {
    vec2 position = v_coords * pixels_scale + pixels_offset;
    vec2 corner = abs(position - size * 0.5) - size * 0.5 + radius;
    float distance = length(max(corner, 0.0)) + min(max(corner.x, corner.y), 0.0) - radius;

    vec4 color = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0);
#endif

    gl_FragColor = color * clamp(0.5 - distance, 0.0, 1.0) * alpha;
}
"#;

/// The shader, compiled once per GPU context.
#[derive(Clone)]
struct Program(GlesTexProgram);

fn program(renderer: &mut GlesRenderer) -> Option<GlesTexProgram> {
    if let Some(program) = renderer.egl_context().user_data().get::<Program>() {
        return Some(program.0.clone());
    }

    let program = renderer
        .compile_custom_texture_shader(
            SHADER,
            &[
                UniformName::new("pixels_scale", UniformType::_2f),
                UniformName::new("pixels_offset", UniformType::_2f),
                UniformName::new("size", UniformType::_2f),
                UniformName::new("radius", UniformType::_1f),
            ],
        )
        .map_err(|err| tracing::warn!("Failed to compile the rounding shader: {err}"))
        .ok()?;

    renderer
        .egl_context()
        .user_data()
        .insert_if_missing(|| Program(program.clone()));
    Some(program)
}

/// What cuts an element to a window's shape.
#[derive(Debug, Clone)]
pub struct Rounding {
    program: GlesTexProgram,
    /// The window on the output.
    window: Rectangle<i32, Physical>,
    radius: f32,
}

impl Rounding {
    /// `None` when there's no shader, in which case windows just stay square.
    pub fn new(
        renderer: &mut GlesRenderer,
        window: Rectangle<i32, Physical>,
        radius: f32,
    ) -> Option<Self> {
        Some(Self {
            program: program(renderer)?,
            window,
            radius,
        })
    }

    fn apply<E>(
        &self,
        inner: E,
        pixels_scale: (f32, f32),
        pixels_offset: (f32, f32),
    ) -> RoundedElement<E> {
        RoundedElement {
            inner,
            program: self.program.clone(),
            uniforms: vec![
                Uniform::new("pixels_scale", pixels_scale),
                Uniform::new("pixels_offset", pixels_offset),
                Uniform::new(
                    "size",
                    (self.window.size.w as f32, self.window.size.h as f32),
                ),
                Uniform::new("radius", self.radius),
            ],
            window: self.window,
            radius: self.radius,
        }
    }

    /// Cuts one of the window's surfaces.
    pub fn surface<R: crate::render::PRenderer>(
        &self,
        element: WaylandSurfaceRenderElement<R>,
        scale: Scale<f64>,
    ) -> RoundedElement<WaylandSurfaceRenderElement<R>> {
        let geometry = element.geometry(scale);
        let buffer = element.buffer_size().to_f64();
        let src = element.view().src;

        self.region(
            element,
            (buffer.w, buffer.h).into(),
            Rectangle::new(
                (src.loc.x, src.loc.y).into(),
                (src.size.w, src.size.h).into(),
            ),
            geometry,
        )
    }

    /// Cuts `inner`, which draws `src` of a texture of `texture_size` to `geometry`.
    pub fn region<E>(
        &self,
        inner: E,
        texture_size: Size<f64, Buffer>,
        src: Rectangle<f64, Buffer>,
        geometry: Rectangle<i32, Physical>,
    ) -> RoundedElement<E> {
        let (width, height) = (geometry.size.w as f64, geometry.size.h as f64);
        let (x, y) = (
            (geometry.loc.x - self.window.loc.x) as f64,
            (geometry.loc.y - self.window.loc.y) as f64,
        );

        self.apply(
            inner,
            (
                (texture_size.w / src.size.w * width) as f32,
                (texture_size.h / src.size.h * height) as f32,
            ),
            (
                (x - src.loc.x / src.size.w * width) as f32,
                (y - src.loc.y / src.size.h * height) as f32,
            ),
        )
    }

    /// Cuts an element that shows the whole window in one texture.
    pub fn whole<E: Element>(&self, inner: E, scale: Scale<f64>) -> RoundedElement<E> {
        let geometry = inner.geometry(scale);
        let window = Self {
            window: geometry,
            ..self.clone()
        };
        window.apply(
            inner,
            (geometry.size.w as f32, geometry.size.h as f32),
            (0.0, 0.0),
        )
    }
}

/// An element cut to a window's shape.
#[derive(Debug)]
pub struct RoundedElement<E> {
    inner: E,
    program: GlesTexProgram,
    uniforms: Vec<Uniform<'static>>,
    window: Rectangle<i32, Physical>,
    radius: f32,
}

impl<E: Element> Element for RoundedElement<E> {
    fn id(&self) -> &element::Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.location(scale)
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        // What's behind the corners shows now.
        let origin = self.inner.geometry(scale).loc;
        let radius = self.radius.ceil() as i32;
        let (left, top) = (self.window.loc.x - origin.x, self.window.loc.y - origin.y);
        let (right, bottom) = (
            left + self.window.size.w - radius,
            top + self.window.size.h - radius,
        );
        let corners = [(left, top), (right, top), (left, bottom), (right, bottom)]
            .map(|(x, y)| Rectangle::new((x, y).into(), (radius, radius).into()));

        Rectangle::subtract_rects_many(self.inner.opaque_regions(scale), corners)
            .into_iter()
            .collect()
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> element::Kind {
        self.inner.kind()
    }
}

impl<E: RenderElement<GlesRenderer>> RenderElement<GlesRenderer> for RoundedElement<E> {
    fn draw(
        &self,
        frame: &mut <GlesRenderer as RendererSuper>::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), <GlesRenderer as RendererSuper>::Error> {
        frame.override_default_tex_program(self.program.clone(), self.uniforms.clone());
        let result = self.inner.draw(frame, src, dst, damage, opaque_regions);
        frame.clear_tex_program_override();
        result
    }
}

impl<'a, E: RenderElement<UdevRenderer<'a>>> RenderElement<UdevRenderer<'a>> for RoundedElement<E> {
    fn draw(
        &self,
        frame: &mut <UdevRenderer<'a> as RendererSuper>::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), <UdevRenderer<'a> as RendererSuper>::Error> {
        frame
            .as_mut()
            .override_default_tex_program(self.program.clone(), self.uniforms.clone());
        let result = self.inner.draw(frame, src, dst, damage, opaque_regions);
        frame.as_mut().clear_tex_program_override();
        result
    }
}

#[cfg(feature = "testing")]
impl<E: RenderElement<DummyRenderer>> RenderElement<DummyRenderer> for RoundedElement<E> {
    fn draw(
        &self,
        frame: &mut <DummyRenderer as RendererSuper>::Frame<'static, 'static>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), <DummyRenderer as RendererSuper>::Error> {
        self.inner.draw(frame, src, dst, damage, opaque_regions)
    }
}
