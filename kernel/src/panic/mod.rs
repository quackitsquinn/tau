//! Panic handling and stack tracing.

use core::{convert::Infallible, fmt::Write, panic::PanicInfo};

use cake::{Fuse, stacktrace};

use crate::{
    declare_module, hlt_loop,
    interrupts::KernelInterrupt,
    memory::{self},
    mp::{self, LAPIC, lapic::icr::IPIDestination},
    print, println,
    serial::{self, interface::SERIAL_PORT_NUM, raw::SerialPort},
    testing,
};

/// A basic panic handler that just prints the panic message to the serial port.
pub fn panic_basic(pi: &PanicInfo) {
    // Write the raw panic message to the serial port.
    let mut panic_writer = unsafe { SerialPort::new(SERIAL_PORT_NUM) };
    if !serial::is_initialized() {
        // If the code crashed before the serial port was initialized, we need to initialize it now.
        panic_writer.init();
    }
    let _ = panic_writer.write_str("Panic: ");
    let _ = if let Some(location) = pi.location() {
        panic_writer.write_fmt(format_args!("{}:{}", location.file(), location.line()))
    } else {
        panic_writer.write_str("Unknown location")
    };
    let _ = panic_writer.write_str("\n");
    let _ = panic_writer.write_fmt(format_args!("{}", pi.message()));
    let _ = panic_writer.write_str("\n");
}

/// A more traditional panic handler that includes more information.
pub fn panic_extended_info(pi: &PanicInfo) {
    println!("=== KERNEL PANIC ===");
    print!("Panic at ");
    if let Some(location) = pi.location() {
        println!("{}:{}", location.file(), location.line());
    } else {
        println!("Unknown location");
    }
    println!();
    println!("{}", pi.message());

    println!("=== STACK TRACE ===");
    panic_stacktrace();
    println!("=== END STACK TRACE ===");
}

pub(crate) fn panic_stacktrace() {
    let stacktrace = stacktrace::collect_stacktrace::<32>();
    stacktrace.print(4, kserial::client::writer());
}

static PANICKED: Fuse = Fuse::new();

/// Default method for handling panics.
/// This will defer to [panic_basic] if a double panic occurs (e.g. a panic within `panic_extended_info`)
pub fn panic(pi: &PanicInfo) -> ! {
    if PANICKED.is_blown() {
        //println!("Double panic!");
        panic_basic(pi);
        hlt_loop();
    }
    PANICKED.blow();

    // Send an IPI to all other cores to notify them of the panic. We don't care about waiting for delivery, because
    // that could potentially result in deadlock if those cores do not have interrupts enabled.
    if mp::is_initialized() {
        LAPIC
            .icr()
            .send(IPIDestination::AllExceptSelf, KernelInterrupt::Panic)
            .ignore();
    }

    panic_extended_info(pi);
    println!("Done; attempting QEMU exit");
    testing::try_shutdown_qemu(true);
    println!("Failed to exit QEMU; halting");
    hlt_loop();
}

declare_module!("panic", init);

fn init() -> Result<(), Infallible> {
    // Nothing to initialize yet, but keeping this here in case we need it later
    Ok(())
}
