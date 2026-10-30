//! Runtime encoder bitrate changes shared by the Linux and Windows capture backends.

use gst::prelude::*;
use gstreamer as gst;

use casement_net::screen::{MAX_VIDEO_BITRATE_KBPS, MIN_VIDEO_BITRATE_KBPS};

pub(crate) fn initial(requested_kbps: u32) -> u32 {
    if requested_kbps == 0 {
        20_000
    } else {
        requested_kbps.clamp(MIN_VIDEO_BITRATE_KBPS, MAX_VIDEO_BITRATE_KBPS)
    }
}

fn property_value(name: &str, bitrate_kbps: u32) -> u64 {
    let bitrate = u64::from(bitrate_kbps.clamp(MIN_VIDEO_BITRATE_KBPS, MAX_VIDEO_BITRATE_KBPS));
    if name == "openh264enc" {
        bitrate.saturating_mul(1000)
    } else {
        bitrate
    }
}

pub(crate) fn set(encoder: &gst::Element, name: &str, bitrate_kbps: u32) -> bool {
    let Some(spec) = encoder.find_property("bitrate") else {
        tracing::warn!(encoder = name, "encoder has no runtime bitrate property");
        return false;
    };
    let raw = property_value(name, bitrate_kbps);
    let accepted = if let Some(number) = spec.downcast_ref::<gst::glib::ParamSpecUInt>() {
        raw >= u64::from(number.minimum()) && raw <= u64::from(number.maximum())
    } else if let Some(number) = spec.downcast_ref::<gst::glib::ParamSpecInt>() {
        raw <= number.maximum().max(0) as u64
    } else if let Some(number) = spec.downcast_ref::<gst::glib::ParamSpecUInt64>() {
        raw >= number.minimum() && raw <= number.maximum()
    } else if let Some(number) = spec.downcast_ref::<gst::glib::ParamSpecInt64>() {
        raw <= number.maximum().max(0) as u64
    } else {
        false
    };
    if !accepted {
        tracing::warn!(
            encoder = name,
            bitrate_kbps,
            "bitrate is outside encoder range"
        );
        return false;
    }
    if spec.is::<gst::glib::ParamSpecUInt>() {
        encoder.set_property("bitrate", raw as u32);
    } else if spec.is::<gst::glib::ParamSpecInt>() {
        encoder.set_property("bitrate", raw as i32);
    } else if spec.is::<gst::glib::ParamSpecUInt64>() {
        encoder.set_property("bitrate", raw);
    } else {
        encoder.set_property("bitrate", raw as i64);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_bitrate_is_safe_but_no_longer_limited_to_100_mbit() {
        assert_eq!(initial(0), 20_000);
        assert_eq!(initial(2_500), MIN_VIDEO_BITRATE_KBPS);
        assert_eq!(initial(800_000), 800_000);
        assert_eq!(initial(u32::MAX), MAX_VIDEO_BITRATE_KBPS);
        assert_eq!(property_value("openh264enc", 800_000), 800_000_000);
    }
}
