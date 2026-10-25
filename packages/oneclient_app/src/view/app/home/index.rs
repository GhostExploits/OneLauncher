use freya::prelude::*;

use crate::components::{ActiveClusterPanel, RecentsRow, SpiderSprite};
use crate::theme;

#[derive(PartialEq)]
pub struct Home;

impl Component for Home {
    fn render(&self) -> impl IntoElement {
        rect()
            .vertical()
            .width(Size::fill())
            .height(Size::fill())
            .content(Content::Flex)
            .overflow(Overflow::Clip)
            .padding(theme::HOME_PADDING_PX)
            .main_align(Alignment::Center)
            // The mark hangs in the quiet corner of the home view. It never
            // overlaps or intercepts the launch controls.
            .child(
                rect().width(Size::fill()).height(Size::px(64.)).child(
                    rect()
                        .position(Position::new_absolute().top(-8.).right(4.))
                        .child(SpiderSprite::new(44.)),
                ),
            )
            .child(
                rect()
                    .vertical()
                    .width(Size::fill())
                    .height(Size::flex(1.0))
                    .main_align(Alignment::Center)
                    .child(ActiveClusterPanel),
            )
            .child(RecentsRow)
    }
}
