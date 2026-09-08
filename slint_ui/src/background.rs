// Copyright (c) 2026 vivo Mobile Communication Co., Ltd.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//       http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use slint::platform::software_renderer::{PremultipliedRgbaColor, Rgb565Pixel, TargetPixel};

/// RGB565 stored in the byte order expected by the CO5300 panel.
#[repr(transparent)]
#[derive(Clone, Copy, Default)]
pub struct PanelRgb565Pixel(pub u16);

impl TargetPixel for PanelRgb565Pixel {
    #[inline]
    fn blend(&mut self, color: PremultipliedRgbaColor) {
        let mut native = Rgb565Pixel(u16::from_be(self.0));
        native.blend(color);
        self.0 = native.0.to_be();
    }

    #[inline]
    fn from_rgb(red: u8, green: u8, blue: u8) -> Self {
        let native = Rgb565Pixel::from_rgb(red, green, blue);
        Self(native.0.to_be())
    }
}

/// Pre-rendered 480 x 480 RGB565 big-endian background.
pub const BACKGROUND_RGB565: &[u8] =
    include_bytes!("../resources/background_480x480.rgb565");

/// Initialize the dirty scanline range before Slint blends foreground items.
#[inline]
pub fn copy_background_line(out: &mut [PanelRgb565Pixel], row: usize, x_start: usize) {
    debug_assert!(row < 480);
    debug_assert!(x_start + out.len() <= 480);

    let byte_count = core::mem::size_of_val(out);
    let source_start = (row * 480 + x_start) * 2;
    let source = &BACKGROUND_RGB565[source_start..source_start + byte_count];
    let destination = unsafe {
        core::slice::from_raw_parts_mut(out.as_mut_ptr().cast::<u8>(), byte_count)
    };
    destination.copy_from_slice(source);
}
