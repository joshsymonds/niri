use crate::appearance::Color;
use crate::utils::{Flag, MergeWith};

/// Configuration for screencast appearance and overlay-filtering policy. Read
/// by the indicator-border render path, the screencast layer-shell filter, and
/// the Zoom auto-hide policy when at least one cast is active. Always parsed,
/// even when the `xdp-gnome-screencast` feature is disabled — the consumers
/// are feature-gated, not the schema.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenCast {
    pub indicator: ScreenCastIndicator,
    pub hide_overlay_layer: bool,
    pub hide_top_layer: bool,
    pub hide_bottom_layer: bool,
    pub hide_background_layer: bool,
    pub hide_zoom_non_shared_windows: bool,
}

impl Default for ScreenCast {
    fn default() -> Self {
        // Defaults set by the screencast-polish epic: hide Top + Overlay
        // layers by default (notifications, panels), keep Bottom + Background
        // visible (wallpaper, desktop widgets). Indicator off until a width
        // is configured. Zoom auto-hide on by default since that is precisely
        // the user-facing motivation for the epic.
        Self {
            indicator: ScreenCastIndicator::default(),
            hide_overlay_layer: true,
            hide_top_layer: true,
            hide_bottom_layer: false,
            hide_background_layer: false,
            hide_zoom_non_shared_windows: true,
        }
    }
}

/// Visual indicator drawn around the currently-cast region on the local
/// screen only. By construction (the renderer gates it on
/// `RenderTarget::Output`), it never appears in cast frames. `width = 0`
/// disables it. `color = None` defers to a renderer-side default.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct ScreenCastIndicator {
    pub width: u16,
    pub color: Option<Color>,
}

#[derive(knuffel::Decode, Debug, Default, PartialEq)]
pub struct ScreenCastPart {
    #[knuffel(child)]
    pub indicator: Option<ScreenCastIndicatorPart>,
    #[knuffel(child)]
    pub hide_overlay_layer: Option<Flag>,
    #[knuffel(child)]
    pub hide_top_layer: Option<Flag>,
    #[knuffel(child)]
    pub hide_bottom_layer: Option<Flag>,
    #[knuffel(child)]
    pub hide_background_layer: Option<Flag>,
    #[knuffel(child)]
    pub hide_zoom_non_shared_windows: Option<Flag>,
}

#[derive(knuffel::Decode, Debug, Default, PartialEq)]
pub struct ScreenCastIndicatorPart {
    #[knuffel(child, unwrap(argument))]
    pub width: Option<u16>,
    #[knuffel(child)]
    pub color: Option<Color>,
}

impl MergeWith<ScreenCastPart> for ScreenCast {
    fn merge_with(&mut self, part: &ScreenCastPart) {
        merge!((self, part), indicator);
        merge!(
            (self, part),
            hide_overlay_layer,
            hide_top_layer,
            hide_bottom_layer,
            hide_background_layer,
            hide_zoom_non_shared_windows,
        );
    }
}

impl MergeWith<ScreenCastIndicatorPart> for ScreenCastIndicator {
    fn merge_with(&mut self, part: &ScreenCastIndicatorPart) {
        merge_clone!((self, part), width);
        merge_clone_opt!((self, part), color);
    }
}

#[cfg(test)]
mod tests {
    use insta::assert_debug_snapshot;

    use crate::Config;

    #[test]
    fn default_hides_overlay_and_top_only() {
        let config = Config::parse_mem("").unwrap();
        let sc = config.screen_cast;
        assert!(sc.hide_overlay_layer, "default hide_overlay_layer is true");
        assert!(sc.hide_top_layer, "default hide_top_layer is true");
        assert!(!sc.hide_bottom_layer, "default hide_bottom_layer is false");
        assert!(
            !sc.hide_background_layer,
            "default hide_background_layer is false"
        );
        assert!(
            sc.hide_zoom_non_shared_windows,
            "default hide_zoom_non_shared_windows is true"
        );
        assert_eq!(sc.indicator.width, 0, "indicator off by default");
        assert!(
            sc.indicator.color.is_none(),
            "indicator color None by default"
        );
    }

    #[test]
    fn parse_indicator_only() {
        let config = Config::parse_mem(
            r##"
            screen-cast {
                indicator {
                    width 2
                }
            }
            "##,
        )
        .unwrap();

        assert_eq!(config.screen_cast.indicator.width, 2);
        assert!(config.screen_cast.indicator.color.is_none());
        // Other defaults still apply.
        assert!(config.screen_cast.hide_overlay_layer);
    }

    #[test]
    fn parse_indicator_with_color() {
        let config = Config::parse_mem(
            r##"
            screen-cast {
                indicator {
                    width 3
                    color "#ff5555"
                }
            }
            "##,
        )
        .unwrap();

        let indicator = config.screen_cast.indicator;
        assert_eq!(indicator.width, 3);
        let color = indicator.color.expect("color set");
        // Just confirm parsing landed in the right approximate place.
        assert!((color.r - 1.0).abs() < 0.01);
        assert!((color.g - 0x55 as f32 / 255.).abs() < 0.01);
        assert!((color.b - 0x55 as f32 / 255.).abs() < 0.01);
    }

    #[test]
    fn parse_each_layer_flag_off() {
        let config = Config::parse_mem(
            r##"
            screen-cast {
                hide-overlay-layer false
                hide-top-layer false
                hide-zoom-non-shared-windows false
            }
            "##,
        )
        .unwrap();

        assert!(!config.screen_cast.hide_overlay_layer);
        assert!(!config.screen_cast.hide_top_layer);
        assert!(!config.screen_cast.hide_zoom_non_shared_windows);
        // Untouched defaults preserved.
        assert!(!config.screen_cast.hide_bottom_layer);
        assert!(!config.screen_cast.hide_background_layer);
    }

    #[test]
    fn parse_each_layer_flag_on() {
        let config = Config::parse_mem(
            r##"
            screen-cast {
                hide-bottom-layer true
                hide-background-layer true
            }
            "##,
        )
        .unwrap();

        assert!(config.screen_cast.hide_bottom_layer);
        assert!(config.screen_cast.hide_background_layer);
        // Untouched defaults preserved.
        assert!(config.screen_cast.hide_overlay_layer);
        assert!(config.screen_cast.hide_top_layer);
    }

    #[test]
    fn parse_flag_without_argument_means_true() {
        // The Flag pattern: bare child without an argument defaults to true.
        let config = Config::parse_mem(
            r##"
            screen-cast {
                hide-bottom-layer
            }
            "##,
        )
        .unwrap();

        assert!(config.screen_cast.hide_bottom_layer);
    }

    #[test]
    fn parse_full() {
        let config = Config::parse_mem(
            r##"
            screen-cast {
                indicator {
                    width 4
                    color "#00ff00"
                }
                hide-overlay-layer false
                hide-top-layer false
                hide-bottom-layer true
                hide-background-layer true
                hide-zoom-non-shared-windows false
            }
            "##,
        )
        .unwrap();

        let sc = config.screen_cast;
        assert_eq!(sc.indicator.width, 4);
        assert!(sc.indicator.color.is_some());
        assert!(!sc.hide_overlay_layer);
        assert!(!sc.hide_top_layer);
        assert!(sc.hide_bottom_layer);
        assert!(sc.hide_background_layer);
        assert!(!sc.hide_zoom_non_shared_windows);
    }

    #[test]
    fn debug_default_is_stable() {
        // Snapshot the defaults so future schema changes are visible in diff.
        assert_debug_snapshot!(crate::ScreenCast::default(), @r"
        ScreenCast {
            indicator: ScreenCastIndicator {
                width: 0,
                color: None,
            },
            hide_overlay_layer: true,
            hide_top_layer: true,
            hide_bottom_layer: false,
            hide_background_layer: false,
            hide_zoom_non_shared_windows: true,
        }
        ");
    }
}
