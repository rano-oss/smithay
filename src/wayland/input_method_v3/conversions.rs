/*! Conversions between text-input types */

use wayland_protocols::wp::text_input::zv3::server::zwp_text_input_v3;
use wl_input_method::text_input::v3::server::xx_text_input_v3::{ChangeCause, ContentHint, ContentPurpose};

/// Converts a type into something that can be consumed by the supported Wayland object.
pub trait ConvertInto<T> {
    fn convert_into(self) -> T;
}

impl<T> ConvertInto<T> for T {
    fn convert_into(self) -> T {
        self
    }
}

impl ConvertInto<ChangeCause> for zwp_text_input_v3::ChangeCause {
    fn convert_into(self) -> ChangeCause {
        match self {
            Self::InputMethod => ChangeCause::InputMethod,
            Self::Other => ChangeCause::Other,
            _ => ChangeCause::Other,
        }
    }
}

impl ConvertInto<ContentHint> for zwp_text_input_v3::ContentHint {
    fn convert_into(self) -> ContentHint {
        ContentHint::from_bits_truncate(self.bits())
    }
}

impl ConvertInto<ContentPurpose> for zwp_text_input_v3::ContentPurpose {
    fn convert_into(self) -> ContentPurpose {
        match self {
            Self::Normal => ContentPurpose::Normal,
            any => ContentPurpose::try_from(any as u32)
                .unwrap_or(ContentPurpose::Normal)
        }
    }
}

impl ConvertInto<zwp_text_input_v3::ChangeCause> for ChangeCause {
    fn convert_into(self) -> zwp_text_input_v3::ChangeCause {
        use zwp_text_input_v3::ChangeCause::*;
        match self {
            Self::InputMethod => InputMethod,
            Self::Other => Other,
            _ => Other,
        }
    }
}

impl ConvertInto<zwp_text_input_v3::ContentHint> for ContentHint {
    fn convert_into(self) -> zwp_text_input_v3::ContentHint {
        zwp_text_input_v3::ContentHint::from_bits_truncate(self.bits())
    }
}

impl ConvertInto<zwp_text_input_v3::ContentPurpose> for ContentPurpose {
    fn convert_into(self) -> zwp_text_input_v3::ContentPurpose {
        match self {
            Self::Normal => zwp_text_input_v3::ContentPurpose::Normal,
            any => zwp_text_input_v3::ContentPurpose::try_from(any as u32)
                .unwrap_or(zwp_text_input_v3::ContentPurpose::Normal)
        }
    }
}