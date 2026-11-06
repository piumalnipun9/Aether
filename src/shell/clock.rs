// SPDX-License-Identifier: GPL-3.0-or-later

//! The panel the clock in the bar opens, and notifications popping up.

use std::{cell::RefCell, rc::Rc, time::Instant};

use chrono::{DateTime, Local};
use slint::{Image, ModelRc, PlatformError, Rgba8Pixel, SharedPixelBuffer, VecModel};
use smithay::{
    backend::renderer::element::memory::MemoryRenderBufferRenderElement,
    input::keyboard::{KeysymHandle, keysyms},
    utils::{Logical, Point, Rectangle},
};

use super::{
    BAR_HEIGHT, ClockPanel, DayItem, NotificationItem, Shell, ShellWindow, Toasts,
    calendar::{self, time_ago},
    set_platform,
    system::Media,
    update_model,
};
use crate::{render::PRenderer, shell::notifications::Notification};

/// The width of the clock panel in logical pixels.
pub const CLOCK_WIDTH: i32 = 720;

/// The most space the clock panel can take up.
pub const CLOCK_MAX_HEIGHT: i32 = 560;

/// The width of notifications popping up in logical pixels.
const TOAST_WIDTH: i32 = 380;

/// The space notifications popping up can take up.
const TOAST_HEIGHT: i32 = 380;

/// How many notifications pop up at the same time.
const MAX_TOASTS: usize = 3;

/// The space between what's here and the bar.
const MARGIN: i32 = 8;

fn notification_item(notification: &Notification, now: DateTime<Local>) -> NotificationItem {
    NotificationItem {
        id: notification.id as i32,
        app: notification.app.as_str().into(),
        initial: notification
            .app
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_default()
            .into(),
        summary: notification.summary.as_str().into(),
        body: notification.body.as_str().into(),
        time: time_ago(notification.received, now).into(),
        critical: notification.critical,
    }
}

/// What was done in the clock panel.
pub enum ClockAction {
    Close,
    ChangeMonth(i32),
    GoToToday,
    ClickNotification(u32),
    DismissNotification(u32),
    ClearNotifications,
    ToggleDoNotDisturb,
    MediaPlayPause,
    MediaPrevious,
    MediaNext,
    MediaSeek(f32),
}

/// The clock panel on an output.
pub struct OutputClock {
    pub ui: ClockPanel,
    pub window: ShellWindow,
    pub blur: RefCell<crate::render::util::blur::Blur>,
    notifications: Rc<VecModel<NotificationItem>>,
    days: Rc<VecModel<DayItem>>,
    actions: Rc<RefCell<Vec<ClockAction>>>,
    /// The album art being shown, by its URL.
    art_url: RefCell<String>,
}

impl std::fmt::Debug for OutputClock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputClock").finish_non_exhaustive()
    }
}

impl OutputClock {
    pub fn new(height: i32, scale: f64) -> Result<Self, PlatformError> {
        set_platform();
        let ui = ClockPanel::new()?;
        let window = ShellWindow::new(&ui, (CLOCK_WIDTH, height).into(), scale)?;

        let notifications = Rc::new(VecModel::default());
        ui.set_notifications(ModelRc::from(notifications.clone()));
        let days = Rc::new(VecModel::default());
        ui.set_days(ModelRc::from(days.clone()));

        let actions = Rc::new(RefCell::new(Vec::new()));
        let queue = |make: fn() -> ClockAction| {
            let actions = actions.clone();
            move || actions.borrow_mut().push(make())
        };
        let by_id = |make: fn(u32) -> ClockAction| {
            let actions = actions.clone();
            move |id: i32| actions.borrow_mut().push(make(id as u32))
        };

        ui.on_today_clicked(queue(|| ClockAction::GoToToday));
        ui.on_cleared(queue(|| ClockAction::ClearNotifications));
        ui.on_do_not_disturb_toggled(queue(|| ClockAction::ToggleDoNotDisturb));
        ui.on_media_play_pause(queue(|| ClockAction::MediaPlayPause));
        ui.on_media_previous(queue(|| ClockAction::MediaPrevious));
        ui.on_media_next(queue(|| ClockAction::MediaNext));
        ui.on_notification_clicked(by_id(ClockAction::ClickNotification));
        ui.on_notification_dismissed(by_id(ClockAction::DismissNotification));
        ui.on_month_changed({
            let actions = actions.clone();
            move |change| actions.borrow_mut().push(ClockAction::ChangeMonth(change))
        });
        ui.on_media_seek({
            let actions = actions.clone();
            move |position| actions.borrow_mut().push(ClockAction::MediaSeek(position))
        });

        ui.set_open(true);

        Ok(Self {
            ui,
            window,
            blur: Default::default(),
            notifications,
            days,
            actions,
            art_url: RefCell::default(),
        })
    }

    pub fn take_actions(&self) -> Vec<ClockAction> {
        std::mem::take(&mut self.actions.borrow_mut())
    }

    /// Where the panel goes on an output of this size: under the clock.
    pub fn location(output_width: i32) -> Point<i32, Logical> {
        ((output_width - CLOCK_WIDTH) / 2, BAR_HEIGHT + MARGIN).into()
    }

    /// The part of the panel that's currently visible, relative to the output.
    pub fn visible_area(&self, output_width: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(
            Self::location(output_width),
            (CLOCK_WIDTH, self.ui.get_panel_height().ceil() as i32).into(),
        )
    }

    pub fn key(&self, key: &KeysymHandle<'_>, pressed: bool) {
        if pressed && self.ui.get_open() && key.modified_sym().raw() == keysyms::KEY_Escape {
            self.actions.borrow_mut().push(ClockAction::Close);
        }
    }

    /// Whether it has finished closing.
    pub fn is_closed(&self) -> bool {
        !self.ui.get_open() && !self.window.is_animating()
    }

    fn set_media(&self, media: Option<&Media>) {
        let ui = &self.ui;
        ui.set_has_media(media.is_some());

        let Some(media) = media else {
            return;
        };

        ui.set_media_title(media.title.as_str().into());
        ui.set_media_artist(media.artist.as_str().into());
        ui.set_media_playing(media.playing);
        ui.set_media_progress(media.progress.unwrap_or(-1.0));

        let mut shown = self.art_url.borrow_mut();
        if *shown != media.art_url {
            shown.clone_from(&media.art_url);
            ui.set_has_media_art(media.art.is_some());
            if let Some(art) = &media.art {
                let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                    art.as_raw(),
                    art.width(),
                    art.height(),
                );
                ui.set_media_art(Image::from_rgba8(buffer));
            }
        }
    }

    pub fn update(&self, shell: &Shell) {
        let ui = &self.ui;
        let now = Local::now();

        ui.set_time(now.format("%H:%M").to_string().into());
        ui.set_date(now.format("%A, %B %-d").to_string().into());

        let (month, days) = calendar::month(now.date_naive(), shell.calendar_offset);
        ui.set_month(month.into());
        update_model(
            &self.days,
            days.into_iter()
                .map(|day| DayItem {
                    day: day.number as i32,
                    today: day.today,
                    in_month: day.in_month,
                })
                .collect(),
        );

        update_model(
            &self.notifications,
            shell
                .notifications
                .iter()
                .rev()
                .map(|notification| notification_item(notification, now))
                .collect(),
        );
        ui.set_do_not_disturb(shell.do_not_disturb);

        self.set_media(shell.system.media.as_ref());
    }
}

/// A notification popping up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Toast {
    pub id: u32,
    /// When it goes away by itself, if it does.
    pub expires: Option<Instant>,
}

/// What was done with a notification popping up.
pub enum ToastAction {
    Click(u32),
    Dismiss(u32),
}

/// Notifications popping up on an output.
pub struct OutputToasts {
    pub ui: Toasts,
    pub window: ShellWindow,
    items: Rc<VecModel<NotificationItem>>,
    actions: Rc<RefCell<Vec<ToastAction>>>,
}

impl std::fmt::Debug for OutputToasts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputToasts").finish_non_exhaustive()
    }
}

impl OutputToasts {
    pub fn new(scale: f64) -> Result<Self, PlatformError> {
        set_platform();
        let ui = Toasts::new()?;
        let window = ShellWindow::new(&ui, (TOAST_WIDTH, TOAST_HEIGHT).into(), scale)?;

        let items = Rc::new(VecModel::default());
        ui.set_toasts(ModelRc::from(items.clone()));

        let actions = Rc::new(RefCell::new(Vec::new()));
        ui.on_clicked({
            let actions = actions.clone();
            move |id| actions.borrow_mut().push(ToastAction::Click(id as u32))
        });
        ui.on_dismissed({
            let actions = actions.clone();
            move |id| actions.borrow_mut().push(ToastAction::Dismiss(id as u32))
        });

        Ok(Self {
            ui,
            window,
            items,
            actions,
        })
    }

    pub fn take_actions(&self) -> Vec<ToastAction> {
        std::mem::take(&mut self.actions.borrow_mut())
    }

    pub fn location(output_width: i32) -> Point<i32, Logical> {
        ((output_width - TOAST_WIDTH) / 2, BAR_HEIGHT + MARGIN).into()
    }

    pub fn visible_area(&self, output_width: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(
            Self::location(output_width),
            (
                TOAST_WIDTH,
                (self.ui.get_content_height().ceil() as i32).min(TOAST_HEIGHT),
            )
                .into(),
        )
    }

    pub fn is_empty(&self) -> bool {
        use slint::Model;
        self.items.row_count() == 0
    }

    pub fn update(&self, shell: &Shell) {
        let now = Local::now();
        update_model(
            &self.items,
            shell
                .toasts
                .iter()
                .rev()
                .take(MAX_TOASTS)
                .filter_map(|toast| shell.notifications.iter().find(|n| n.id == toast.id))
                .map(|notification| notification_item(notification, now))
                .collect(),
        );
    }

    pub fn render_element<R: PRenderer>(
        &self,
        renderer: &mut R,
        location: Point<i32, smithay::utils::Physical>,
    ) -> Option<MemoryRenderBufferRenderElement<R>> {
        self.window.render_element(renderer, location)
    }
}
