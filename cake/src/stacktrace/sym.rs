use core::fmt::Write;

use crate::KERNEL_ELF;

/// Resolves a symbol name for the given address.
pub fn resolve_sym(addr: *const ()) -> Option<&'static str> {
    let elf = crate::kernel_elf()?;

    let sym = elf.symbols()?.iter().find(|s| {
        let sym_addr = s.st_value as usize;
        let sym_size = s.st_size as usize;
        let addr_usize = addr as usize;

        addr_usize >= sym_addr && addr_usize < sym_addr + sym_size
    })?;

    let strtab = elf.strings()?;
    unsafe { strtab.get_str(sym.st_name as usize).ok() }
}

/// Resolves a symbol name for the given address and demangles it if possible.
pub fn resolve_sym_demangle(addr: *const ()) -> Option<impl core::fmt::Display> {
    Some(rustc_demangle::demangle(resolve_sym(addr)?))
}
