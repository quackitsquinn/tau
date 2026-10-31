//! Display output.
use core::convert::Infallible;

use cake::{
    Mutex, OnceMutex,
    limine::{self, response::FramebufferResponse},
};
use framebuffer::Framebuffer;

pub mod character;
pub mod color;
mod framebuffer;
pub mod screen_char;
pub mod terminal;

pub use character::get_char;

use crate::{bootloader, declare_module};

/// The global framebuffer instance.
pub static FRAMEBUFFER: OnceMutex<Framebuffer> = OnceMutex::uninitialized();
/// The global terminal instance.
pub static TERMINAL: OnceMutex<terminal::Terminal> = OnceMutex::uninitialized();

declare_module!("display", init);

fn init() -> Result<(), Infallible> {
    todo!();
    // FRAMEBUFFER.call_init(|| unsafe { Framebuffer::new(bootloader::FRAMEBUFFER.get()) });
    TERMINAL.call_init(|| terminal::Terminal::new(1, 2));
    Ok(())
}

/// Gets the global terminal instance.
#[macro_export]
macro_rules! terminal {
    () => {
        $crate::display::TERMINAL.get()
    };
}

/// Gets the global framebuffer instance.
#[macro_export]
macro_rules! framebuffer {
    () => {
        $crate::display::FRAMEBUFFER.get()
    };
}

/// Information about the framebuffer.
#[derive(Debug)]
pub struct FramebufferInfo {
    /// The width of the framebuffer.
    pub width: u64,
    /// The height of the framebuffer.
    pub height: u64,
    /// The pitch (e.g. the number of bytes in a row) of the framebuffer.
    pub pitch: u64,
    /// The number of **bits** (*not bytes*) per pixel.
    pub bpp: u16,
    ptr: Mutex<(bool, *mut u8)>,
}

impl FramebufferInfo {
    /// Creates a new `FramebufferInfo` from the given Limine framebuffer response.
    pub fn new(fb_data: &limine::framebuffer::Framebuffer<'_>) -> Self {
        Self {
            width: fb_data.width(),
            height: fb_data.height(),
            pitch: fb_data.pitch(),
            bpp: fb_data.bpp(),
            ptr: Mutex::new((false, fb_data.addr() as *mut u8)),
        }
    }

    /// Update the inner pointer with a new value.
    pub unsafe fn update_ptr(&self, ptr: *mut u8) {
        *self.ptr.lock() = (true, ptr);
    }

    /// Get a raw pointer to the framebuffer.
    pub fn ptr(&self) -> *mut u8 {
        let s = self.ptr.lock();

        if !s.0 {
            panic!("Framebuffer pointer not initialized");
        }

        s.1
    }

    /// Get a raw pointer to the framebuffer without checking if it's initialized.
    /// # Safety
    /// The caller must ensure that the pointer is still a valid pointer to the framebuffer.
    pub unsafe fn ptr_unchecked(&self) -> *mut u8 {
        self.ptr.lock().1
    }
}

unsafe impl Send for FramebufferInfo {}
unsafe impl Sync for FramebufferInfo {}
