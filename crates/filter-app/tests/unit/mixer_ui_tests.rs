use super::*;
use filter_core::color::{linear_srgb_to_rec2020, rec2020_to_oklab};

fn bands_with(value: f32) -> [HslBand; 8] {
    std::array::from_fn(|i| HslBand {
        hue: value,
        saturation: value / 2.0 + i as f32 * 0.01,
        lightness: -value,
    })
}

#[test]
fn percent_text_round_trips_and_clamps() {
    assert_eq!(percent(0.0), "+0");
    assert_eq!(percent(0.256), "+26");
    assert_eq!(percent(-1.0), "-100");
    assert_eq!(parse_percent(" 50 "), Some(0.5));
    assert_eq!(parse_percent("-250"), Some(-1.0));
    assert_eq!(parse_percent("abc"), None);
    assert_eq!(parse_percent("inf"), None);
}
#[test]
fn channel_reset_clears_only_that_channel() {
    let mut bands = bands_with(0.5);
    reset_channel(&mut bands, Channel::Saturation);
    for (i, band) in bands.iter().enumerate() {
        assert_eq!(
            (band.hue, band.saturation, band.lightness),
            (0.5, 0.0, -0.5),
            "band {i}"
        );
    }
    let mut bands = bands_with(0.5);
    reset_channel(&mut bands, Channel::Hue);
    assert!(bands.iter().all(|b| b.hue == 0.0 && b.lightness == -0.5));
}
#[test]
fn tabs_expose_their_channels() {
    assert_eq!(Tab::Hue.channels(), [Channel::Hue]);
    assert_eq!(Tab::Luminance.channels(), [Channel::Luminance]);
    assert_eq!(Tab::All.channels(), Channel::ALL);
    assert_eq!(Tab::All.channel(), None);
    let mut band = HslBand::default();
    *Channel::Luminance.slot(&mut band) = 0.25;
    assert_eq!(band.lightness, 0.25);
}
#[test]
fn swatches_lie_in_gamut_near_their_band_hue() {
    for (k, degrees) in BAND_CENTER_DEGREES.iter().enumerate() {
        let color = band_color(k);
        let linear = [color.r(), color.g(), color.b()]
            .map(|v| filter_core::color::srgb_to_linear(f32::from(v) / 255.0));
        let [_, a, b] = rec2020_to_oklab(linear_srgb_to_rec2020(linear));
        assert!(a.hypot(b) > 0.05, "{} is vivid", BAND_NAMES[k]);
        let hue = b.atan2(a).to_degrees().rem_euclid(360.0);
        let distance = (hue - degrees + 180.0).rem_euclid(360.0) - 180.0;
        assert!(
            distance.abs() < 3.0,
            "{}: {hue} vs {degrees}",
            BAND_NAMES[k]
        );
        let [gray, vivid] = track_colors(Channel::Saturation, k);
        assert!((i32::from(gray.r()) - i32::from(gray.b())).abs() < 4);
        assert_ne!(gray, vivid);
    }
}
