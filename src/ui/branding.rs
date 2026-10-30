//! RubyChat's logo (logo-a, generated with the local Ideogram service): a
//! magenta faceted ruby shaped like a speech bubble with cyan edges. The
//! embedded asset is 256 px, keyed to transparency by
//! `tools/gen_logo.py`; it is resized once per device size.

use crate::render::Image;

/// 256 × 256 straight RGBA.
static LOGO_256: &[u8] = include_bytes!("../../assets/branding/logo-256.rgba");
const SOURCE_SIZE: u32 = 256;

pub struct Logo {
    source: Image,
    sized: Option<Image>,
}

impl Default for Logo {
    fn default() -> Self {
        Self {
            source: Image::from_straight_rgba(SOURCE_SIZE, SOURCE_SIZE, LOGO_256)
                .expect("the embedded logo is 256 × 256 RGBA"),
            sized: None,
        }
    }
}

impl Logo {
    /// The logo `pixels` square, resized on first use at that size.
    pub fn at(&mut self, pixels: u32) -> &Image {
        let pixels = pixels.max(1);
        if self
            .sized
            .as_ref()
            .is_none_or(|image| image.width != pixels)
        {
            self.sized = Some(self.source.resized(pixels, pixels));
        }
        self.sized.as_ref().expect("just sized")
    }

    /// The window icon: straight RGBA at `pixels` square.
    pub fn icon(&self, pixels: u32) -> (Vec<u8>, u32) {
        (
            self.source.resized(pixels, pixels).to_straight_rgba(),
            pixels,
        )
    }
}
