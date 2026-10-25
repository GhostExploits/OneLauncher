use freya::prelude::*;
use freya::router::RouterContext;

use super::{section_header, settings_page, settings_row};
use crate::components::{Button, Icon, IconType, SpiderSprite};
use crate::hooks::use_link_confirm;
use crate::routes::Route;
use crate::theme::colors;
use crate::ui::border_all_color;

const SOURCE_URL: &str = "https://github.com/GhostExploits/OneLauncher";
const ATTRIBUTION_URL: &str =
    "https://github.com/GhostExploits/OneLauncher/blob/main/ATTRIBUTION.md";

#[derive(PartialEq)]
pub struct SettingsAbout;

impl Component for SettingsAbout {
    fn render(&self) -> impl IntoElement {
        let external = use_link_confirm();
        let version = env!("CARGO_PKG_VERSION");

        let mut source_link = external;
        let mut attribution_link = external;

        settings_page()
            .child(
                rect()
                    .horizontal()
                    .width(Size::fill())
                    .cross_align(Alignment::End)
                    .spacing(26.)
                    .padding(Gaps::new(18., 4., 22., 4.))
                    .child(SpiderSprite::new(76.))
                    .child(
                        rect()
                            .vertical()
                            .spacing(2.)
                            .child(
                                label()
                                    .text("SpiderClient")
                                    .font_size(34.)
                                    .font_weight(FontWeight::SEMI_BOLD)
                                    .color(colors::fg_primary()),
                            )
                            .child(
                                label()
                                    .text(format!("Version {version}"))
                                    .font_size(13.)
                                    .color(colors::fg_secondary()),
                            )
                            .child(
                                label()
                                    .text("A focused Minecraft launcher built for real sessions.")
                                    .font_size(13.)
                                    .color(colors::fg_secondary()),
                            ),
                    ),
            )
            .child(section_header("CREDITS"))
            .child(settings_row(
                IconType::Users01,
                "Zach",
                "Project owner and client development.",
                label()
                    .text("Owner")
                    .font_size(12.)
                    .color(colors::fg_secondary()),
            ))
            .child(settings_row(
                IconType::Globe01,
                "Faris",
                "Website development and collaboration.",
                label()
                    .text("Web")
                    .font_size(12.)
                    .color(colors::fg_secondary()),
            ))
            .child(section_header("OPEN SOURCE"))
            .child(
                rect()
                    .vertical()
                    .width(Size::fill())
                    .spacing(10.)
                    .padding(Gaps::new_all(18.))
                    .corner_radius(CornerRadius::new_all(12.))
                    .background(colors::page_elevated())
                    .border(border_all_color(1., colors::component_border()))
                    .child(
                        label()
                            .text("SpiderClient is built on OneLauncher")
                            .font_size(16.)
                            .font_weight(FontWeight::MEDIUM)
                            .color(colors::fg_primary()),
                    )
                    .child(
                        label()
                            .text(
                                "The launcher and its libraries are distributed under GPL-3.0-only. \
                                 Upstream copyright notices for OneLauncher, OneClient, Polyfrost, and \
                                 their contributors remain in effect. SpiderClient artwork and the \
                                 remote monitoring components are original additions.",
                            )
                            .font_size(12.)
                            .width(Size::fill())
                            .color(colors::fg_secondary()),
                    )
                    .child(
                        rect()
                            .horizontal()
                            .spacing(8.)
                            .child(
                                Button::new()
                                    .secondary()
                                    .small()
                                    .on_press(move |_| {
                                        source_link.set(Some(SOURCE_URL.to_string()));
                                    })
                                    .child(Icon::new(IconType::Github).size(15.))
                                    .text("Source"),
                            )
                            .child(
                                Button::new()
                                    .ghost()
                                    .small()
                                    .on_press(move |_| {
                                        attribution_link.set(Some(ATTRIBUTION_URL.to_string()));
                                    })
                                    .text("Attribution & licenses"),
                            ),
                    ),
            )
            .child(section_header("DATA & PRIVACY"))
            .child(settings_row(
                IconType::Database01,
                "Your data stays local by default",
                "Remote monitoring is opt-in, requires an explicit device pairing, and can be \
                 revoked at any time. The launcher remains fully usable when the remote service is \
                 offline.",
                Button::new()
                    .ghost()
                    .small()
                    .on_press(|_| {
                        let _ = RouterContext::get().push(Route::SettingsLauncher {});
                    })
                    .text("Privacy settings"),
            ))
            .into_element()
    }
}
