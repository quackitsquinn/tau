use core::{
    arch::asm,
    fmt::{self, Debug, Display, Write},
};

use arrayvec::ArrayVec;

pub mod sym;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StackFrame {
    pub instruction_pointer: *const (),
    pub last_frame: *mut (),
}

/// A stack trace, consisting of a fixed-size array of stack frames.
#[derive(Clone, PartialEq, Eq)]
pub struct StackTrace<const LIMIT: usize> {
    frames: ArrayVec<StackFrame, LIMIT>,
}

impl<const LIMIT: usize> Debug for StackTrace<LIMIT> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "StackTrace{:?}", &*self.frames)
    }
}

impl<const LIMIT: usize> Display for StackTrace<LIMIT> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.print(0, f)
    }
}

impl<const LIMIT: usize> StackTrace<LIMIT> {
    /// Prints the stack trace to the given writer, skipping the first `skip_levels` frames.
    pub fn print(&self, skip_levels: usize, writer: impl Write) -> fmt::Result {
        let mut writer = writer;

        for (i, frame) in self.frames.iter().enumerate() {
            if i < skip_levels {
                continue;
            }

            let addr = frame.instruction_pointer;
            let symbol = sym::resolve_sym_demangle(addr);

            match symbol {
                Some(sym) => {
                    writeln!(writer, "{} {:p}: {:#}", i - skip_levels, addr, sym)?;
                }
                None => {
                    writeln!(writer, "{} {:p}: <unknown>", i - skip_levels, addr)?;
                }
            }
        }

        Ok(())
    }
}
/// Returns a stack trace of the current call stack, up to a maximum of `LIMIT` frames.
#[inline(never)]
pub fn collect_stacktrace<const LIMIT: usize>() -> StackTrace<LIMIT> {
    let mut frames = ArrayVec::new();
    let mut current_frame = match read_caller_frame(1) {
        None => return StackTrace { frames },
        Some(rip) => rip.last_frame,
    };

    while !current_frame.is_null() && frames.len() < LIMIT {
        if let Some(frame) = unsafe { read_frame(current_frame) } {
            frames.push(frame);
            current_frame = frame.last_frame;
        } else {
            break;
        }
    }

    StackTrace { frames }
}

/// Returns the instruction pointer of the caller at the given level in the call stack.
/// Level 0 is the immediate caller, level 1 is the caller's caller, and so on. Returns `None` if the level is out of bounds.
#[inline(never)]
pub fn read_caller_frame(level: usize) -> Option<StackFrame> {
    let mut curr = root_frame();
    for _ in 0..level + 1 {
        if let Some(frame) = unsafe { read_frame(curr) } {
            curr = frame.last_frame;
        } else {
            return None;
        }
    }
    unsafe { read_frame(curr) }
}

/// Returns the root frame of the current thread's call stack.
#[inline(never)]
#[cfg(target_arch = "x86_64")]
pub fn root_frame() -> *mut () {
    let mut rbp: *mut ();
    unsafe {
        asm!("mov {}, rbp", out(reg) rbp);
        // We want the caller's frame, not ours. Traverse a frame up.
        read_frame(rbp).map_or(core::ptr::null_mut(), |frame| frame.last_frame)
    }
}

/// Returns the root frame of the current thread's call stack.
#[cfg(target_arch = "aarch64")]
pub fn root_frame() -> *mut () {
    let mut fp: *mut ();
    unsafe {
        asm!("mov {}, x29", out(reg) fp);
        // We want the caller's frame, not ours. Traverse a frame up.
        read_frame(fp).map_or(core::ptr::null_mut(), |frame| frame.last_frame)
    }
}

#[cfg(target_arch = "x86_64")]
pub unsafe fn read_frame(frame: *const ()) -> Option<StackFrame> {
    #[derive(Clone, Copy)]
    #[repr(C)]
    struct X86StackFrame {
        rbp: *const X86StackFrame,
        rip: *const (),
    }

    if frame.is_null() {
        return None;
    }

    let frame = &unsafe { *(frame as *const X86StackFrame) };

    Some(StackFrame {
        instruction_pointer: frame.rip,
        last_frame: frame.rbp as *mut (),
    })
}

#[cfg(target_arch = "aarch64")]
pub unsafe fn read_frame(frame: *const ()) -> Option<StackFrame> {
    #[derive(Clone, Copy)]
    #[repr(C)]
    struct AArch64StackFrame {
        fp: *const AArch64StackFrame,
        lr: *const (),
    }
    if frame.is_null() {
        return None;
    }

    let frame = &unsafe { *(frame as *const AArch64StackFrame) };

    Some(StackFrame {
        instruction_pointer: frame.lr,
        last_frame: frame.fp as *mut (),
    })
}

pub unsafe fn write_frame(frame: *mut (), new_frame: StackFrame) {
    #[cfg(target_arch = "x86_64")]
    {
        #[derive(Clone, Copy)]
        #[repr(C)]
        struct X86StackFrame {
            rbp: *const X86StackFrame,
            rip: *const (),
        }

        let frame = &mut unsafe { *(frame as *mut X86StackFrame) };
        frame.rbp = new_frame.last_frame as *const X86StackFrame;
        frame.rip = new_frame.instruction_pointer;
    }

    #[cfg(target_arch = "aarch64")]
    {
        #[derive(Clone, Copy)]
        #[repr(C)]
        struct AArch64StackFrame {
            fp: *const AArch64StackFrame,
            lr: *const (),
        }

        let frame = &mut unsafe { *(frame as *mut AArch64StackFrame) };
        frame.fp = new_frame.last_frame as *const AArch64StackFrame;
        frame.lr = new_frame.instruction_pointer;
    }
}
