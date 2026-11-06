// SPDX-License-Identifier: GPL-3.0-or-later

//! Time-based animations with Material 3 easing.
//!
//! Animations are driven by the render loop: while any animation is running,
//! the backends keep scheduling frames (see [`Pinnacle::tick_animations`]).

use std::time::{Duration, Instant};

use smithay::utils::{Logical, Point, Size};
use tracing::debug;

use crate::{
    state::{Pinnacle, WithState},
    window::UnmappingWindow,
};

/// A cubic bézier easing curve from (0, 0) to (1, 1), like CSS `cubic-bezier()`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Curve {
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
}

impl Curve {
    /// Material 3 "emphasized decelerate": things entering the screen.
    pub const EMPHASIZED_DECELERATE: Curve = Curve::new(0.05, 0.7, 0.1, 1.0);
    /// Material 3 "emphasized accelerate": things leaving the screen.
    pub const EMPHASIZED_ACCELERATE: Curve = Curve::new(0.3, 0.0, 0.8, 0.15);
    /// Material 3 "standard": things moving within the screen.
    pub const STANDARD: Curve = Curve::new(0.2, 0.0, 0.0, 1.0);

    pub const fn new(x1: f64, y1: f64, x2: f64, y2: f64) -> Self {
        Self { x1, y1, x2, y2 }
    }

    fn sample(a: f64, b: f64, t: f64) -> f64 {
        // Bézier with endpoints 0 and 1 and control points a and b.
        let u = 1.0 - t;
        3.0 * u * u * t * a + 3.0 * u * t * t * b + t * t * t
    }

    fn sample_derivative(a: f64, b: f64, t: f64) -> f64 {
        let u = 1.0 - t;
        3.0 * u * u * a + 6.0 * u * t * (b - a) + 3.0 * t * t * (1.0 - b)
    }

    /// Maps linear progress `x` in [0, 1] to eased progress.
    pub fn ease(&self, x: f64) -> f64 {
        let x = x.clamp(0.0, 1.0);
        if x == 0.0 || x == 1.0 {
            return x;
        }

        // Solve sample(x1, x2, t) == x for t: Newton's method, then bisection as a fallback.
        let mut t = x;
        for _ in 0..8 {
            let err = Self::sample(self.x1, self.x2, t) - x;
            if err.abs() < 1e-6 {
                return Self::sample(self.y1, self.y2, t);
            }
            let d = Self::sample_derivative(self.x1, self.x2, t);
            if d.abs() < 1e-6 {
                break;
            }
            t -= err / d;
        }

        let (mut lo, mut hi) = (0.0, 1.0);
        t = x;
        for _ in 0..30 {
            let v = Self::sample(self.x1, self.x2, t);
            if (v - x).abs() < 1e-6 {
                break;
            }
            if v < x {
                lo = t;
            } else {
                hi = t;
            }
            t = (lo + hi) / 2.0;
        }
        Self::sample(self.y1, self.y2, t)
    }
}

/// Durations, following Material 3 motion tokens.
pub mod duration {
    use std::time::Duration;

    /// Window opening.
    pub const OPEN: Duration = Duration::from_millis(350);
    /// Window closing.
    pub const CLOSE: Duration = Duration::from_millis(200);
    /// Tiled windows moving to a new place in the layout.
    pub const MOVE: Duration = Duration::from_millis(350);
    /// Sliding between workspaces.
    pub const WORKSPACE: Duration = Duration::from_millis(400);
}

/// A running animation from 0.0 to 1.0.
#[derive(Debug, Clone, Copy)]
pub struct Animation {
    start: Instant,
    duration: Duration,
    curve: Curve,
}

impl Animation {
    /// Starts an animation now.
    pub fn new(duration: Duration, curve: Curve) -> Self {
        Self {
            start: Instant::now(),
            duration,
            curve,
        }
    }

    /// Linear progress in [0, 1].
    fn linear(&self, now: Instant) -> f64 {
        if self.duration.is_zero() {
            return 1.0;
        }
        (now.saturating_duration_since(self.start).as_secs_f64() / self.duration.as_secs_f64())
            .min(1.0)
    }

    /// Eased progress in [0, 1] at the current time.
    pub fn value(&self) -> f64 {
        self.curve.ease(self.linear(Instant::now()))
    }

    /// Uneased progress in [0, 1] at the current time.
    pub fn elapsed_fraction(&self) -> f64 {
        self.linear(Instant::now())
    }

    pub fn is_done(&self) -> bool {
        self.linear(Instant::now()) >= 1.0
    }
}

/// A window's open animation.
///
/// It starts when the window is first drawn, not when it is created, because
/// a new window takes a moment to commit its first buffer and be laid out.
#[derive(Debug, Clone, Copy, Default)]
pub enum OpenAnimation {
    /// Not animating.
    #[default]
    None,
    /// Will start on the next frame the window is drawn in.
    Pending,
    Running(Animation),
}

/// A tiled window gliding and stretching to its new place and size in the layout.
#[derive(Debug, Clone, Copy)]
pub struct MoveAnimation {
    pub anim: Animation,
    /// Where the window was drawn, relative to its new location.
    pub from_offset: Point<f64, Logical>,
    /// The size it was drawn at, relative to its new size, per axis.
    pub from_scale: (f64, f64),
}

impl MoveAnimation {
    /// The offset to draw the window at right now.
    pub fn offset(&self) -> Point<f64, Logical> {
        self.from_offset.upscale(1.0 - self.anim.value())
    }

    /// The scale to draw the window at right now, about its top-left corner.
    pub fn scale(&self) -> (f64, f64) {
        let p = self.anim.value();
        let lerp = |from: f64| from + (1.0 - from) * p;
        (lerp(self.from_scale.0), lerp(self.from_scale.1))
    }
}

/// Values for drawing a window that is opening or closing.
#[derive(Debug, Clone, Copy)]
pub struct Appearance {
    pub alpha: f32,
    /// Scale about the window's center.
    pub scale: f64,
    /// Alpha of the window's border, which appears after the window has.
    pub border_alpha: f32,
}

impl Appearance {
    pub const IDENTITY: Appearance = Appearance {
        alpha: 1.0,
        scale: 1.0,
        border_alpha: 1.0,
    };

    /// The smallest a window gets while opening or closing.
    const MIN_SCALE: f64 = 0.9;

    /// When the border starts fading in, as a fraction of the open animation.
    ///
    /// The decelerating open curve has the window nearly in place by this point.
    const BORDER_DELAY: f64 = 0.6;

    pub fn opening(progress: f64) -> Self {
        Self {
            alpha: progress as f32,
            scale: Self::MIN_SCALE + (1.0 - Self::MIN_SCALE) * progress,
            border_alpha: 0.0,
        }
    }

    /// The border's alpha given the uneased progress of the open animation.
    fn border_alpha_at(elapsed_fraction: f64) -> f32 {
        let fade = (elapsed_fraction - Self::BORDER_DELAY) / (1.0 - Self::BORDER_DELAY);
        fade.clamp(0.0, 1.0) as f32
    }

    pub fn closing(progress: f64) -> Self {
        Self::opening(1.0 - progress)
    }

    pub fn is_identity(&self) -> bool {
        self.alpha >= 1.0 && self.scale >= 1.0
    }
}

impl crate::window::WindowElement {
    /// How to draw this window's open animation right now.
    ///
    /// Starts a pending open animation, so call this only when drawing the window.
    pub fn open_appearance(&self) -> Appearance {
        self.with_state_mut(|state| match state.open_animation {
            OpenAnimation::None => Appearance::IDENTITY,
            OpenAnimation::Pending => {
                debug!(window = state.id.0, "open animation started");
                state.open_animation = OpenAnimation::Running(Animation::new(
                    duration::OPEN,
                    Curve::EMPHASIZED_DECELERATE,
                ));
                Appearance::opening(0.0)
            }
            OpenAnimation::Running(anim) => Appearance {
                border_alpha: Appearance::border_alpha_at(anim.elapsed_fraction()),
                ..Appearance::opening(anim.value())
            },
        })
    }

    /// The extra offset to draw this window at while it moves in the layout.
    pub fn move_offset(&self) -> Point<f64, Logical> {
        self.with_state(|state| {
            state
                .move_animation
                .map(|anim| anim.offset())
                .unwrap_or_default()
        })
    }

    /// The stretch to draw this window at while it resizes in the layout, about its top-left.
    pub fn move_scale(&self) -> (f64, f64) {
        self.with_state(|state| {
            state
                .move_animation
                .map(|anim| anim.scale())
                .unwrap_or((1.0, 1.0))
        })
    }

    /// Starts gliding this window from `old` to `new` (location, size).
    ///
    /// If it is already animating, it continues from where and how big it is drawn now.
    pub fn start_move_animation(
        &self,
        (old_loc, old_size): (Point<i32, Logical>, Size<i32, Logical>),
        (new_loc, new_size): (Point<i32, Logical>, Size<i32, Logical>),
    ) {
        if new_size.w <= 0 || new_size.h <= 0 {
            return;
        }

        let (cur_sx, cur_sy) = self.move_scale();
        let from_offset = (old_loc - new_loc).to_f64() + self.move_offset();
        let from_scale = (
            old_size.w as f64 * cur_sx / new_size.w as f64,
            old_size.h as f64 * cur_sy / new_size.h as f64,
        );

        let moved = from_offset.x.abs() >= 1.0 || from_offset.y.abs() >= 1.0;
        let resized = (from_scale.0 - 1.0).abs() > 0.01 || (from_scale.1 - 1.0).abs() > 0.01;
        if !moved && !resized {
            return;
        }

        self.with_state_mut(|state| {
            debug!(
                window = state.id.0,
                ?from_offset,
                ?from_scale,
                "move animation started"
            );
            state.move_animation = Some(MoveAnimation {
                anim: Animation::new(duration::MOVE, Curve::STANDARD),
                from_offset,
                from_scale,
            });
        });
    }
}

impl UnmappingWindow {
    /// How to draw this snapshot right now.
    pub fn appearance(&self) -> Appearance {
        match self.close_animation {
            Some(anim) => Appearance::closing(anim.value()),
            None => Appearance::IDENTITY,
        }
    }
}

/// A workspace switch sliding the old workspace out and the new one in.
#[derive(Debug)]
pub struct WorkspaceSwitch {
    pub anim: Animation,
    /// 1.0 when the new workspace comes in from the right, -1.0 from the left.
    pub direction: f64,
    /// Snapshots of the old workspace's windows and their output-relative locations.
    pub old_windows: Vec<(
        crate::render::util::snapshot::WindowSnapshot,
        Point<i32, Logical>,
    )>,
}

impl WorkspaceSwitch {
    /// Horizontal offsets for the (old, new) workspace, given the output width.
    pub fn offsets(&self, width: i32) -> (f64, f64) {
        let p = self.anim.value();
        let w = width as f64 * self.direction;
        (-w * p, w * (1.0 - p))
    }
}

impl Pinnacle {
    /// Drops finished animations and returns whether any are still running.
    ///
    /// The backends keep rendering frames while this returns true.
    pub fn tick_animations(&mut self) -> bool {
        let _span = tracy_client::span!("Pinnacle::tick_animations");

        // What just finished still needs a frame of its own to go from what's on screen,
        // which is the second to last step of it, to where it ended up. Without that, it
        // stays on screen like that until something else causes a render.
        let mut running = false;

        for window in self.windows.iter() {
            window.with_state_mut(|state| {
                match state.open_animation {
                    OpenAnimation::Running(anim) if anim.is_done() => {
                        state.open_animation = OpenAnimation::None;
                        running = true;
                    }
                    OpenAnimation::None => (),
                    _ => running = true,
                }
                match state.move_animation {
                    Some(anim) if anim.anim.is_done() => {
                        state.move_animation = None;
                        running = true;
                    }
                    Some(_) => running = true,
                    None => (),
                }
            });
        }

        let closing = self.closing_windows.len();

        self.closing_windows
            .retain(|closing| closing.close_animation.is_some_and(|anim| !anim.is_done()));
        running |= closing > 0;

        for output in self.outputs.iter() {
            output.with_state_mut(|state| {
                running |= state.workspace_switch.is_some();
                if state
                    .workspace_switch
                    .as_ref()
                    .is_some_and(|sw| sw.anim.is_done())
                {
                    state.workspace_switch = None;
                }
                running |= state
                    .bar
                    .as_ref()
                    .is_some_and(|bar| bar.window.is_animating())
                    || state
                        .panel
                        .as_ref()
                        .is_some_and(|panel| panel.window.is_animating())
                    || state
                        .clock
                        .as_ref()
                        .is_some_and(|clock| clock.window.is_animating())
                    || state
                        .toasts
                        .as_ref()
                        .is_some_and(|toasts| toasts.window.is_animating())
                    || state
                        .launcher
                        .as_ref()
                        .is_some_and(|launcher| launcher.window.is_animating());
            });
        }

        running
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_hit_endpoints_and_are_monotonic() {
        for curve in [
            Curve::EMPHASIZED_DECELERATE,
            Curve::EMPHASIZED_ACCELERATE,
            Curve::STANDARD,
        ] {
            assert_eq!(curve.ease(0.0), 0.0);
            assert_eq!(curve.ease(1.0), 1.0);
            let mut last = 0.0;
            for i in 1..=100 {
                let v = curve.ease(i as f64 / 100.0);
                assert!(v + 1e-9 >= last, "{curve:?} not monotonic at {i}");
                last = v;
            }
        }
    }

    #[test]
    fn border_appears_after_the_window() {
        assert_eq!(Appearance::border_alpha_at(0.0), 0.0);
        assert_eq!(Appearance::border_alpha_at(Appearance::BORDER_DELAY), 0.0);
        assert!((Appearance::border_alpha_at(0.8) - 0.5).abs() < 1e-3);
        assert_eq!(Appearance::border_alpha_at(1.0), 1.0);
        assert!(Curve::EMPHASIZED_DECELERATE.ease(Appearance::BORDER_DELAY) > 0.9);
    }

    #[test]
    fn decelerate_is_fast_at_start() {
        assert!(Curve::EMPHASIZED_DECELERATE.ease(0.2) > 0.5);
        assert!(Curve::EMPHASIZED_ACCELERATE.ease(0.5) < 0.2);
    }
}
