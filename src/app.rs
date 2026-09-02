// SPDX-License-Identifier: GPL-3.0-only

//! The window: a navigation rail on the left, one page per tab on the right.

use cosmic::app::{Core, Settings, Task};
use cosmic::iced::Length;
use cosmic::prelude::*;
use cosmic::widget::{self, nav_bar};

use crate::fl;

/// The tabs, in the order the rail lists them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Page {
    Processes,
    Performance,
    Users,
}

#[derive(Clone, Debug)]
pub enum Message {}

pub struct App {
    core: Core,
    nav: nav_bar::Model,
}

impl App {
    fn page(&self) -> Page {
        self.nav.active_data::<Page>().copied().unwrap_or(Page::Processes)
    }
}

impl cosmic::Application for App {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;

    /// Matches the basename of `res/fun.wmde.taskmgr.desktop` and its `Icon=`: the toolkit
    /// hands this out as the Wayland app id, and that is the only thing tying the running
    /// window to its entry and icon.
    const APP_ID: &'static str = "fun.wmde.taskmgr";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, _flags: Self::Flags) -> (Self, Task<Self::Message>) {
        let mut nav = nav_bar::Model::default();
        nav.insert()
            .text(fl!("processes"))
            .icon(widget::icon::from_name("view-list-symbolic"))
            .data(Page::Processes);
        nav.insert()
            .text(fl!("performance"))
            .icon(widget::icon::from_name("utilities-system-monitor-symbolic"))
            .data(Page::Performance);
        nav.insert()
            .text(fl!("users"))
            .icon(widget::icon::from_name("system-users-symbolic"))
            .data(Page::Users);
        nav.activate_position(0);

        let mut app = App { core, nav };
        let task = app.set_window_title(fl!("app-title"), app.core.main_window_id().unwrap());
        (app, task)
    }

    fn nav_model(&self) -> Option<&nav_bar::Model> {
        Some(&self.nav)
    }

    fn on_nav_select(&mut self, id: nav_bar::Id) -> Task<Self::Message> {
        self.nav.activate(id);
        Task::none()
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {}
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let body = match self.page() {
            Page::Processes => fl!("processes"),
            Page::Performance => fl!("performance"),
            Page::Users => fl!("users"),
        };

        widget::container(widget::text::body(body))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

pub fn run() -> cosmic::iced::Result {
    let settings = Settings::default().size_limits(
        cosmic::iced::Limits::NONE
            .min_width(360.0)
            .min_height(180.0),
    );
    cosmic::app::run::<App>(settings, ())
}
