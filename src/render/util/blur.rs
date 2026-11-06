// SPDX-License-Identifier: GPL-3.0-or-later

//! Blurring what's behind the shell's panels.
//!
//! The scene gets drawn small, which already does much of the work and keeps this
//! cheap, and is then blurred with a few Kawase passes. The result is cut to the panel's
//! rounded rectangle, so that the element on screen is an ordinary texture.

use anyhow::Context;
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, Frame, Offscreen, Renderer, Texture,
            element::{
                Kind, RenderElement,
                texture::{TextureBuffer, TextureRenderElement},
            },
            gles::{GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName, UniformType},
        },
    },
    utils::{Buffer, Physical, Point, Rectangle, Scale, Size, Transform},
};

use super::render_elements_to_framebuffer;
use crate::render::texture::CommonTextureRenderElement;

/// How much smaller than the output the scene is drawn.
pub const DOWNSCALE: f64 = 4.0;

/// How far each pass samples, in texels of the small scene.
const PASSES: [f32; 4] = [0.5, 1.5, 2.5, 2.5];

const BLUR_SHADER: &str = r#"#version 100
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision mediump float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;
uniform vec2 offset;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

void main() {
    vec4 color = texture2D(tex, v_coords + offset)
        + texture2D(tex, v_coords - offset)
        + texture2D(tex, v_coords + vec2(offset.x, -offset.y))
        + texture2D(tex, v_coords + vec2(-offset.x, offset.y));

    gl_FragColor = color * 0.25 * alpha;
}
"#;

const ROUND_SHADER: &str = r#"#version 100
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
// Where the region is in the texture, as an origin and a size.
uniform vec4 region;
// The size of the result in pixels.
uniform vec2 size;
uniform float radius;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

void main() {
    vec2 position = (v_coords - region.xy) / region.zw * size;
    vec2 corner = abs(position - size * 0.5) - size * 0.5 + radius;
    float distance = length(max(corner, 0.0)) + min(max(corner.x, corner.y), 0.0) - radius;

    gl_FragColor = texture2D(tex, v_coords) * clamp(0.5 - distance, 0.0, 1.0) * alpha;
}
"#;

/// The shaders, compiled once per GPU context.
#[derive(Clone)]
struct Programs {
    blur: GlesTexProgram,
    round: GlesTexProgram,
}

fn programs(renderer: &mut GlesRenderer) -> anyhow::Result<Programs> {
    if let Some(programs) = renderer.egl_context().user_data().get::<Programs>() {
        return Ok(programs.clone());
    }

    let programs = Programs {
        blur: renderer.compile_custom_texture_shader(
            BLUR_SHADER,
            &[UniformName::new("offset", UniformType::_2f)],
        )?,
        round: renderer.compile_custom_texture_shader(
            ROUND_SHADER,
            &[
                UniformName::new("region", UniformType::_4f),
                UniformName::new("size", UniformType::_2f),
                UniformName::new("radius", UniformType::_1f),
            ],
        )?,
    };
    renderer
        .egl_context()
        .user_data()
        .insert_if_missing(|| programs.clone());
    Ok(programs)
}

/// Draws `src` over all of `dst`.
fn pass(
    renderer: &mut GlesRenderer,
    src: &GlesTexture,
    src_region: Rectangle<f64, Buffer>,
    dst: &mut GlesTexture,
    program: &GlesTexProgram,
    uniforms: &[Uniform<'_>],
) -> anyhow::Result<()> {
    let size = dst.size();
    let size = Size::<i32, Physical>::from((size.w, size.h));
    let rect = Rectangle::from_size(size);

    let mut framebuffer = renderer.bind(dst).context("failed to bind texture")?;
    let mut frame = renderer
        .render(&mut framebuffer, size, Transform::Normal)
        .context("failed to start render")?;

    frame.clear([0.0, 0.0, 0.0, 0.0].into(), &[rect])?;
    frame.render_texture_from_to(
        src,
        src_region,
        rect,
        &[rect],
        &[],
        Transform::Normal,
        1.0,
        Some(program),
        uniforms,
    )?;
    let _ = frame.finish()?;

    Ok(())
}

/// The size the scene is blurred at.
fn small_size(output_size: Size<i32, Physical>) -> Size<i32, Physical> {
    (
        (output_size.w as f64 / DOWNSCALE).ceil() as i32,
        (output_size.h as f64 / DOWNSCALE).ceil() as i32,
    )
        .into()
}

/// Textures kept between frames, because a panel is blurred on every one of them.
#[derive(Debug, Default)]
pub struct Blur {
    scene: Option<[GlesTexture; 2]>,
    result: Option<GlesTexture>,
}

impl Blur {
    /// Blurs `elements`, which go from the bottom up and were created with the scale
    /// of the output divided by [`DOWNSCALE`], into a texture of that size.
    pub fn render_scene(
        &mut self,
        renderer: &mut GlesRenderer,
        elements: impl IntoIterator<Item = impl RenderElement<GlesRenderer>>,
        output_size: Size<i32, Physical>,
        small_scale: Scale<f64>,
    ) -> anyhow::Result<GlesTexture> {
        let programs = programs(renderer)?;
        self.blur_scene(renderer, &programs, elements, output_size, small_scale)
    }

    /// Blurs `elements`, which go from the bottom up and were created with the scale
    /// of the output divided by [`DOWNSCALE`], and cuts out `region` of the output with
    /// rounded corners.
    pub fn render(
        &mut self,
        renderer: &mut GlesRenderer,
        elements: impl IntoIterator<Item = impl RenderElement<GlesRenderer>>,
        output_size: Size<i32, Physical>,
        region: Rectangle<i32, Physical>,
        radius: f32,
        small_scale: Scale<f64>,
    ) -> anyhow::Result<CommonTextureRenderElement> {
        let programs = programs(renderer)?;

        let front = self.blur_scene(renderer, &programs, elements, output_size, small_scale)?;
        let small = small_size(output_size);

        let result_size = region.size.to_logical(1).to_buffer(1, Transform::Normal);
        if self
            .result
            .as_ref()
            .is_none_or(|result| result.size() != result_size)
        {
            self.result = Some(renderer.create_buffer(Fourcc::Abgr8888, result_size)?);
        }
        let result = self.result.as_mut().expect("just set");

        let src = Rectangle::<f64, Buffer>::new(
            (
                region.loc.x as f64 / DOWNSCALE,
                region.loc.y as f64 / DOWNSCALE,
            )
                .into(),
            (
                region.size.w as f64 / DOWNSCALE,
                region.size.h as f64 / DOWNSCALE,
            )
                .into(),
        );
        let in_texture = (
            (src.loc.x / small.w as f64) as f32,
            (src.loc.y / small.h as f64) as f32,
            (src.size.w / small.w as f64) as f32,
            (src.size.h / small.h as f64) as f32,
        );

        pass(
            renderer,
            &front,
            src,
            result,
            &programs.round,
            &[
                Uniform::new("region", in_texture),
                Uniform::new("size", (region.size.w as f32, region.size.h as f32)),
                Uniform::new("radius", radius),
            ],
        )?;

        let buffer =
            TextureBuffer::from_texture(renderer, result.clone(), 1, Transform::Normal, None);
        Ok(CommonTextureRenderElement::new(
            TextureRenderElement::from_texture_buffer(
                Point::<f64, Physical>::from((region.loc.x as f64, region.loc.y as f64)),
                &buffer,
                None,
                None,
                None,
                Kind::Unspecified,
            ),
        ))
    }

    /// Draws `elements` small and blurs them, and gives back the result.
    fn blur_scene(
        &mut self,
        renderer: &mut GlesRenderer,
        programs: &Programs,
        elements: impl IntoIterator<Item = impl RenderElement<GlesRenderer>>,
        output_size: Size<i32, Physical>,
        small_scale: Scale<f64>,
    ) -> anyhow::Result<GlesTexture> {
        let small = small_size(output_size);
        let small_buffer = small.to_logical(1).to_buffer(1, Transform::Normal);

        let reusable = self
            .scene
            .as_ref()
            .is_some_and(|[scene, _]| scene.size() == small_buffer);
        if !reusable {
            let mut texture = || renderer.create_buffer(Fourcc::Abgr8888, small_buffer);
            self.scene = Some([texture()?, texture()?]);
        }
        let [front, back] = self.scene.as_mut().expect("just set");

        {
            let mut framebuffer = renderer.bind(front).context("failed to bind texture")?;
            let _ = render_elements_to_framebuffer(
                renderer,
                &mut framebuffer,
                elements,
                small,
                small_scale,
                Transform::Normal,
            )?;
        }

        let whole = Rectangle::from_size(small_buffer.to_f64());
        for distance in PASSES {
            let offset = (distance / small.w as f32, distance / small.h as f32);
            pass(
                renderer,
                front,
                whole,
                back,
                &programs.blur,
                &[Uniform::new("offset", offset)],
            )?;
            std::mem::swap(front, back);
        }

        Ok(front.clone())
    }
}
