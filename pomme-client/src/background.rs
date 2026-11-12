//! Background mode (`--background`, or `POMME_BACKGROUND` set to anything
//! but `0`): the client runs without getting in the way of whoever is at
//! the machine. Its window opens off the screen, is never activated and
//! stays off the taskbar, and the client leaves the pointer alone. Test
//! runs use it, so a harness script can play out while the desktop is in
//! use; it still renders every frame, so screenshots come out the same.

use std::sync::atomic::{AtomicBool, Ordering};

static ON: AtomicBool = AtomicBool::new(false);

/// Where the window is put: far outside any monitor, like a minimized
/// window's parking spot but still drawn to.
pub const OFF_SCREEN: winit::dpi::PhysicalPosition<i32> =
    winit::dpi::PhysicalPosition::new(-20_000, -20_000);

/// Turns background mode on when the launch asks for it.
pub fn init(flag: bool) {
    let env = std::env::var_os("POMME_BACKGROUND").is_some_and(|v| !v.is_empty() && v != "0");
    if flag || env {
        ON.store(true, Ordering::Relaxed);
        tracing::info!("Background mode: the window stays off the screen and unfocused");
    }
}

pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}
