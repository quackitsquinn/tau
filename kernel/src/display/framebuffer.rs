use core::fmt::Debug;

use cake::{limine, log::info};

use super::color::Color;
/// A representation of a framebuffer.
pub struct Framebuffer {
    width: usize,
    height: usize,
    pitch: usize,
    /// Bytes per pixel.
    bpp: u16,
    // We could of used a 2D array, but because of pitch we can't.
    /// The frame buffer.
    buffer: &'static mut [u8],
}

impl Framebuffer {
    /// Create a new framebuffer.
    pub unsafe fn new(fb: &limine::framebuffer::Framebuffer<'_>) -> Framebuffer {
        if fb.bpp() % 8 != 0 {
            panic!("Non-byte aligned framebuffers are not supported.");
        } else if fb.bpp() / 8 < 3 {
            panic!("Framebuffers with less than 3 bytes per pixel are not supported.");
        }

        info!(
            "Framebuffer: {}x{} {}bpp ({})",
            fb.width(),
            fb.height(),
            fb.bpp(),
            fb.pitch() * fb.height()
        );
        Self {
            width: fb.width() as usize,
            height: fb.height() as usize,
            pitch: fb.pitch() as usize,
            bpp: (fb.bpp() / 8),
            buffer: unsafe {
                // Safety: We calculate the buffer size based on the pitch and height of the framebuffer.
                core::slice::from_raw_parts_mut(
                    fb.addr(),
                    fb.pitch() as usize * fb.height() as usize,
                )
            },
        }
    }
    /// Get the width of the framebuffer.
    pub fn width(&self) -> usize {
        self.width
    }
    /// Get the height of the framebuffer.
    pub fn height(&self) -> usize {
        self.height
    }
    /// Get the pitch of the framebuffer. (The number of bytes per row of the framebuffer.)
    pub fn pitch(&self) -> usize {
        self.pitch
    }
    /// Gets the underlying buffer.
    pub fn buffer(&mut self) -> &mut [u8] {
        self.buffer
    }
    /// Set a pixel at a specific location.
    #[inline]
    pub fn set_px(&mut self, x: usize, y: usize, color: Color) {
        assert!(x < self.width && y < self.height, "Pixel out of bounds");
        let offset = (y * self.pitch) + (x * (self.bpp as usize));
        color.to_slice(&mut self.buffer[offset..offset + self.bpp as usize]);
    }
    /// Draws a scaled pixel at a specific location.
    /// The origin is the top left corner.
    #[inline]
    pub fn draw_scaled_px(&mut self, x: usize, y: usize, scale: usize, color: Color) {
        for i in 0..scale {
            for j in 0..scale {
                self.set_px(x + i, y + j, color);
            }
        }
    }

    /// Draws a scaled 8xn sprite at a specific location.
    /// The origin is the top left corner.
    #[inline]
    pub fn draw_sprite(
        &mut self,
        x: usize,
        y: usize,
        scale: usize,
        sprite: &[u8],
        foreground: Color,
        background: Color,
    ) {
        for (i, byte) in sprite.iter().enumerate() {
            for bit in 0..8 {
                if byte & (1 << bit) != 0 {
                    self.draw_scaled_px(x + bit * scale, y + (i % 8) * scale, scale, foreground);
                } else {
                    self.draw_scaled_px(x + bit * scale, y + (i % 8) * scale, scale, background);
                }
            }
        }
    }

    /// Clears the framebuffer with a specific color.
    pub fn clear(&mut self) {
        self.buffer.fill(0);
    }

    /// Gets the size of the framebuffer.
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }
}

impl Debug for Framebuffer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Framebuffer")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("pitch", &self.pitch)
            .field("bpp", &self.bpp)
            .finish()
    }
}
