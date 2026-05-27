//! On-screen indicator that something is being cast.
//!
//! Rendered ONLY on `RenderTarget::Output` (gated at the call site in
//! `src/niri.rs`), so it is impossible for the indicator to appear in the
//! cast stream itself — that is the entire safety property of this feature.
//!
//! This module covers the OUTPUT-cast case (a border inset from the
//! output's edge). The window-cast case is a separate decoration on `Tile`
//! and lives next to focus-ring rendering.

use niri_config::{Color, CornerRadius, GradientInterpolation, ScreenCastIndicator};
use smithay::output::Output;
use smithay::utils::{Logical, Point, Rectangle, Size};

use crate::render_helpers::border::BorderRenderElement;
#[cfg(feature = "xdp-gnome-screencast")]
use crate::screencasting::ActiveCasts;

/// Fallback indicator color when the user hasn't set `indicator { color "..." }`.
/// Matches the example in `resources/default-config.kdl`.
pub const DEFAULT_INDICATOR_COLOR: Color =
    Color::new_unpremul(1.0, 85.0 / 255.0, 85.0 / 255.0, 1.0);

/// Build screencast-indicator border elements for `output`. Returns empty when
/// no monitor cast targets this output OR the indicator is disabled.
///
/// `output_size` is the output's logical size; `scale` is the output's
/// fractional scale.
#[cfg(feature = "xdp-gnome-screencast")]
pub fn output_indicator_elements(
    active_casts: &ActiveCasts,
    output: &Output,
    config: &ScreenCastIndicator,
    output_size: Size<f64, Logical>,
    scale: f64,
) -> Vec<BorderRenderElement> {
    if config.width == 0 {
        return Vec::new();
    }
    if !active_casts.contains_output(&output.name()) {
        return Vec::new();
    }

    let color = config.color.unwrap_or(DEFAULT_INDICATOR_COLOR);
    let width_logical = f64::from(config.width);
    let width_f32 = config.width as f32;

    // The border is drawn AROUND `geometry`, within bounding `size`.
    // To get a border inset from the output edge, set size = whole output and
    // geometry = the inner rect (inset by width on each side). The result is a
    // border of `width` thickness sitting just inside the output edge.
    let size = output_size;
    let inner_size = Size::from((
        (output_size.w - width_logical * 2.0).max(0.0),
        (output_size.h - width_logical * 2.0).max(0.0),
    ));
    let geometry = Rectangle::new(Point::from((width_logical, width_logical)), inner_size);
    let area = Rectangle::new(Point::from((0.0, 0.0)), size);

    let element = BorderRenderElement::new(
        size,
        area, // gradient_area — same as full rect, doesn't matter for solid color
        GradientInterpolation::default(),
        color,
        color,
        0.0, // angle — irrelevant for solid color
        geometry,
        width_f32,
        CornerRadius::default(),
        scale as f32,
        1.0,
    );

    vec![element]
}

#[cfg(test)]
#[cfg(feature = "xdp-gnome-screencast")]
mod tests {
    use niri_config::ScreenCastIndicator;
    use smithay::output::{PhysicalProperties, Subpixel};

    use super::*;

    fn mock_output(name: &str) -> Output {
        Output::new(
            name.to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".to_string(),
                model: "test".to_string(),
                serial_number: "0".to_string(),
            },
        )
    }

    fn mock_size() -> Size<f64, Logical> {
        Size::from((1920.0, 1080.0))
    }

    #[test]
    fn no_indicator_when_inactive() {
        let active = ActiveCasts::default();
        let output = mock_output("HDMI-A-1");
        let config = ScreenCastIndicator {
            width: 2,
            color: None,
        };
        let elements = output_indicator_elements(&active, &output, &config, mock_size(), 1.0);
        assert!(elements.is_empty());
    }

    #[test]
    fn no_indicator_when_width_zero() {
        let mut active = ActiveCasts::default();
        active.outputs.insert("HDMI-A-1".to_string());
        let output = mock_output("HDMI-A-1");
        let config = ScreenCastIndicator {
            width: 0,
            color: Some(Color::new_unpremul(1.0, 0.0, 0.0, 1.0)),
        };
        let elements = output_indicator_elements(&active, &output, &config, mock_size(), 1.0);
        assert!(elements.is_empty());
    }

    #[test]
    fn indicator_for_matching_output() {
        let mut active = ActiveCasts::default();
        active.outputs.insert("HDMI-A-1".to_string());
        let output = mock_output("HDMI-A-1");
        let config = ScreenCastIndicator {
            width: 2,
            color: None,
        };
        let elements = output_indicator_elements(&active, &output, &config, mock_size(), 1.0);
        assert_eq!(elements.len(), 1);
    }

    #[test]
    fn no_indicator_for_unmatched_output() {
        let mut active = ActiveCasts::default();
        active.outputs.insert("HDMI-A-1".to_string());
        let output = mock_output("DP-2");
        let config = ScreenCastIndicator {
            width: 2,
            color: None,
        };
        let elements = output_indicator_elements(&active, &output, &config, mock_size(), 1.0);
        assert!(elements.is_empty());
    }

    #[test]
    fn indicator_ignored_when_window_cast_only() {
        // Window-cast indicators are NOT handled here; they live on Tile.
        let mut active = ActiveCasts::default();
        active.windows.insert(42);
        let output = mock_output("HDMI-A-1");
        let config = ScreenCastIndicator {
            width: 2,
            color: None,
        };
        let elements = output_indicator_elements(&active, &output, &config, mock_size(), 1.0);
        assert!(elements.is_empty());
    }
}
