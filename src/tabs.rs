// SPDX-License-Identifier: GPL-3.0-only

//! The look of the tab strip in the title bar.
//!
//! A copy of `wmde_tab_style` from `wmde-term/src/main.rs`, which is where this look was
//! settled and where it is documented ([.doc/lists](../../../.doc/lists/README.md)). Copied
//! rather than shared because the original lives in a binary crate with no library half; if a
//! third consumer appears, move it into libcosmic instead of copying it again.
//!
//! The variant taken is the one the terminal uses **inside a pane**, not the one it uses in
//! the title bar. The title-bar variant drops the active-tab underline because the strip merges
//! into the menu row painted the same colour, and an underline there would only draw a line
//! across that seam. There is no menu row here, so the hint stays.

use cosmic::iced::{self, Color};
use cosmic::theme::style;
use cosmic::widget::segmented_button::{Appearance, ItemAppearance, ItemStatusAppearance};

/// The colour a tab is painted, and the colour a menu row would be painted beside it.
fn body(theme: &cosmic::Theme) -> Color {
    Color::from(theme.cosmic().primary(theme.transparent).base)
}

#[must_use]
pub fn style() -> style::SegmentedButton {
    style::SegmentedButton::Custom(Box::new(move |theme| {
        let cosmic = theme.cosmic();
        let rad = cosmic.corner_radii.radius_s;
        let top_round = iced::border::Radius::from([rad[0], rad[1], 0.0, 0.0]);
        let body = body(theme);
        let on = Color::from(cosmic.background(theme.transparent).on);

        // Nudge the fill towards the text colour - lighter in a dark theme, darker in a light
        // one. A translucent copy of the fill would have made the ACTIVE tab look weaker under
        // the pointer, because libcosmic draws the hover appearance instead of the active one
        // for whichever tab is hovered, active or not.
        //
        // The hover step is 0.20, the same nominal strength every other list in WMDE hovers
        // with (`text_button.hover`, neutral_5 at 20 %). The technique differs because a tab
        // has to stay opaque, but the strength does not.
        let toward_ink = |t: f32| Color {
            r: body.r + (on.r - body.r) * t,
            g: body.g + (on.g - body.g) * t,
            b: body.b + (on.b - body.b) * t,
            a: 1.0,
        };
        let item = |radius| ItemAppearance {
            border: iced::Border {
                radius,
                ..Default::default()
            },
        };
        let active = ItemStatusAppearance {
            background: Some(iced::Background::Color(body)),
            first: item(top_round),
            middle: item(top_round),
            last: item(top_round),
            text_color: on,
        };
        Appearance {
            background: None,
            border: iced::Border::default(),
            active_width: 4.0,
            active,
            inactive: ItemStatusAppearance {
                background: None,
                // Dim the same ink rather than switch to component.on, which is DARKER than
                // background.on in the light palette and would invert the emphasis there.
                text_color: Color { a: 0.65, ..on },
                ..active
            },
            hover: ItemStatusAppearance {
                background: Some(iced::Background::Color(toward_ink(0.20))),
                text_color: on,
                ..active
            },
            pressed: ItemStatusAppearance {
                background: Some(iced::Background::Color(toward_ink(0.26))),
                text_color: on,
                ..active
            },
        }
    }))
}
