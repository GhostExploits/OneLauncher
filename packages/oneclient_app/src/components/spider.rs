use freya::animation::{AnimNum, Function, OnCreation, OnFinish, use_animation};
use freya::prelude::*;

use crate::{AppAssets, hooks::use_settings_snapshot, theme::colors};

/// One full hang, wink, and retract. Long enough to be calm, short enough to
/// notice while the launcher is starting.
const CYCLE_MS: u64 = 7_200;

/// Original monochrome SpiderClient mark.
///
/// The animation is deliberately decorative and cheap: one animated number
/// moves two positioned nodes and swaps a pre-embedded wink frame. Disabling
/// animations in Settings leaves the static mark in place.
#[derive(PartialEq)]
pub struct SpiderSprite {
    size: f32,
}

impl SpiderSprite {
    pub fn new(size: f32) -> Self {
        Self { size }
    }
}

impl Component for SpiderSprite {
    fn render(&self) -> impl IntoElement {
        let animations = use_settings_snapshot().settings.animations_enabled;

        let hang = use_animation(|conf| {
            conf.on_creation(OnCreation::Run);
            conf.on_finish(OnFinish::restart());
            AnimNum::new(0., 1.)
                .time(CYCLE_MS)
                .function(Function::Linear)
        });

        let p = hang.get().value();
        let (drop, wink, visible) = if animations {
            // Descend, hang, wink once, retract, then wait off-screen before
            // the next pass so the loop does not feel mechanical.
            let drop = if p < 0.04 {
                -1.0
            } else if p < 0.34 {
                -1.0 + smoothstep((p - 0.04) / 0.30)
            } else if p < 0.62 {
                0.0
            } else if p < 0.92 {
                -(smoothstep((p - 0.62) / 0.30))
            } else {
                -1.0
            };
            let wink = (0.47..0.535).contains(&p);
            (drop, wink, p > 0.015 && p < 0.99)
        } else {
            (0.0, false, true)
        };

        let size = self.size;
        let top = drop * (size + 8.0);
        let thread_height = (top + size * 0.27).max(0.0);
        let marked = use_memo(|| {
            (
                AppAssets::get_bytes("spider-mark.svg").unwrap_or_default(),
                AppAssets::get_bytes("spider-mark-wink.svg").unwrap_or_default(),
            )
        });
        let (open_bytes, wink_bytes) = (*marked.read()).clone();
        let bytes = if wink { wink_bytes } else { open_bytes };

        rect()
            .width(Size::px(size))
            .height(Size::px(size + 12.0))
            .overflow(Overflow::None)
            .opacity(if visible { 1.0 } else { 0.0 })
            // The thread is drawn separately so the mark can travel without
            // stretching the spider itself.
            .child(
                rect()
                    .position(Position::new_absolute().top(0.).left(size / 2. - 0.5))
                    .width(Size::px(1.))
                    .height(Size::px(thread_height))
                    .background(colors::fg_primary().with_a(72)),
            )
            .child(
                rect()
                    .position(Position::new_absolute().top(top).left(0.))
                    .width(Size::px(size))
                    .height(Size::px(size))
                    .child(
                        SvgViewer::new((
                            if wink {
                                "spider-mark-wink.svg"
                            } else {
                                "spider-mark.svg"
                            },
                            bytes,
                        ))
                        .show_loader(false)
                        .width(Size::px(size))
                        .height(Size::px(size))
                        .color(colors::fg_primary()),
                    ),
            )
    }
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    t * t * (3. - 2. * t)
}
