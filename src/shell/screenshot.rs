// SPDX-License-Identifier: GPL-3.0-or-later

//! Taking screenshots.
//!
//! The screen is captured right away, so that what's on it stays as it was while
//! choosing what to keep. The selection is then cut from that.

use std::{cell::RefCell, rc::Rc, sync::Arc};

use image::RgbaImage;
use slint::PlatformError;
use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    input::keyboard::{KeysymHandle, keysyms},
    utils::{Logical, Physical, Size, Transform},
};

use super::{ScreenshotOverlay, ShellWindow, capture::Area, set_platform};

/// What was done with the overlay.
pub enum ScreenshotAction {
    Cancel,
    /// Take this part of the output, or all of it.
    Take(Option<Area>),
}

/// The overlay on an output, and what it captured.
pub struct OutputScreenshot {
    pub ui: ScreenshotOverlay,
    pub window: ShellWindow,
    /// The captured screen, to stay on screen.
    pub frozen: MemoryRenderBuffer,
    pub pixel_size: Size<i32, Physical>,
    image: Arc<RgbaImage>,
    actions: Rc<RefCell<Vec<ScreenshotAction>>>,
}

impl std::fmt::Debug for OutputScreenshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputScreenshot").finish_non_exhaustive()
    }
}

impl OutputScreenshot {
    pub fn new(
        image: RgbaImage,
        size: Size<i32, Logical>,
        scale: f64,
    ) -> Result<Self, PlatformError> {
        set_platform();
        let ui = ScreenshotOverlay::new()?;
        let window = ShellWindow::new(&ui, size, scale)?;

        let pixel_size = Size::from((image.width() as i32, image.height() as i32));
        let frozen = MemoryRenderBuffer::from_slice(
            image.as_raw(),
            Fourcc::Abgr8888,
            (pixel_size.w, pixel_size.h),
            1,
            Transform::Normal,
            None,
        );

        let actions = Rc::new(RefCell::new(Vec::new()));
        ui.on_finished({
            let actions = actions.clone();
            move |x, y, width, height| {
                actions.borrow_mut().push(ScreenshotAction::Take(Some((
                    x.into(),
                    y.into(),
                    width.into(),
                    height.into(),
                ))));
            }
        });

        Ok(Self {
            ui,
            window,
            frozen,
            pixel_size,
            image: Arc::new(image),
            actions,
        })
    }

    pub fn take_actions(&self) -> Vec<ScreenshotAction> {
        std::mem::take(&mut self.actions.borrow_mut())
    }

    pub fn image(&self) -> Arc<RgbaImage> {
        self.image.clone()
    }

    pub fn key(&self, key: &KeysymHandle<'_>, pressed: bool) {
        if !pressed {
            return;
        }
        match key.modified_sym().raw() {
            keysyms::KEY_Escape => self.actions.borrow_mut().push(ScreenshotAction::Cancel),
            keysyms::KEY_Return | keysyms::KEY_KP_Enter => {
                self.actions.borrow_mut().push(ScreenshotAction::Take(None));
            }
            _ => (),
        }
    }
}
