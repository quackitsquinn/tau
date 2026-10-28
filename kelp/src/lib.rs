//! Kernel ELF Parser
//!
//! This crate provides a simple ELF parser for the kernel, allowing you to read sections,
//! symbols, strings, and segments from an ELF file.

#![no_std]
#![feature(mut_restriction)]

use core::fmt::Debug;

use goblin::{
    elf::section_header::SHT_SYMTAB,
    elf64::{
        header,
        program_header::ProgramHeader,
        reloc::SIZEOF_RELA,
        section_header,
        sym::{SIZEOF_SYM, Sym},
    },
};

pub use goblin;

pub use crate::{reloc::ElfRelocation, sections::ElfSections, strings::ElfStrings};

mod reloc;
mod sections;
mod strings;

/// An ELF executable
pub struct Elf<'a> {
    /// The raw ELF data
    pub mut(crate) data: &'a [u8],
    header: &'a header::Header,
}
/// An error that can occur while parsing an ELF file.
#[derive(thiserror::Error, Debug)]
pub enum ElfError {
    /// The ELF file does not contain enough data.
    #[error("Not enough data: {actual} < {expected}")]
    NotEnoughData {
        /// The actual size of the data.
        actual: usize,
        /// The expected size of the data.
        expected: usize,
    },
    /// The ELF file has an invalid magic number.
    #[error("Invalid magic: {actual:?} != {expected:?}")]
    InvalidMagic {
        /// The actual magic number.
        actual: [u8; 4],
        /// The expected magic number.
        expected: [u8; 4],
    },
    /// The ELF file has an invalid architecture.
    #[error("Invalid architecture: {actual} != {expected}")]
    InvalidArchitecture {
        /// The actual architecture.
        actual: u8,
        /// The expected architecture.
        expected: u8,
    },
}

impl<'a> Elf<'a> {
    /// Create a ELF executable from data
    pub fn new(data: &'a [u8]) -> Result<Elf<'a>, ElfError> {
        if data.len() < header::SIZEOF_EHDR {
            Err(ElfError::NotEnoughData {
                actual: data.len(),
                expected: header::SIZEOF_EHDR,
            })
        } else if &data[..header::SELFMAG] != header::ELFMAG {
            let mut ret = [0u8; 4];
            ret.clone_from_slice(&data[..header::SELFMAG]);
            Err(ElfError::InvalidMagic {
                actual: ret,
                expected: *header::ELFMAG,
            })
        } else if data.get(header::EI_CLASS) != Some(&header::ELFCLASS) {
            Err(ElfError::InvalidArchitecture {
                actual: data[header::EI_CLASS],
                expected: header::ELFCLASS,
            })
        } else {
            Ok(Elf {
                data,
                header: unsafe { &*(data.as_ptr().cast()) },
            })
        }
    }

    /// Iterate over the sections in the ELF file.
    pub fn sections(&'a self) -> ElfSections<'a> {
        unsafe { ElfSections::new(self.data, self.header) }
    }

    /// Returns the symbols in the ELF file as a slice.
    pub fn symbols(&'a self) -> Option<&'a [Sym]> {
        let symtab = self
            .sections()
            .find(|section| section.sh_type == SHT_SYMTAB)?;

        let size = symtab.sh_size as usize / SIZEOF_SYM;
        let offset = symtab.sh_offset as usize;

        Some(unsafe {
            core::slice::from_raw_parts(self.data.as_ptr().add(offset).cast::<Sym>(), size)
        })
    }

    /// Returns the segments in the ELF file as a slice.
    pub fn segments(&'a self) -> &'a [ProgramHeader] {
        let size = self.header.e_phnum as usize;
        let offset = self.header.e_phoff as usize;

        unsafe {
            core::slice::from_raw_parts(
                self.data.as_ptr().add(offset).cast::<ProgramHeader>(),
                size,
            )
        }
    }

    /// Returns the string table in the ELF file.
    pub fn strings(&'a self) -> Option<ElfStrings<'a>> {
        Some(unsafe { ElfStrings::new(self.data, self) })
    }

    /// Returns the relocations in the ELF file as a slice.
    pub fn relocations(&'a self) -> Option<&'a [ElfRelocation]> {
        let reloc_section = self
            .sections()
            .find(|section| section.sh_type == section_header::SHT_RELA)?;

        let size = reloc_section.sh_size as usize / SIZEOF_RELA;
        let offset = reloc_section.sh_offset as usize;

        Some(unsafe {
            core::slice::from_raw_parts(
                self.data.as_ptr().add(offset).cast::<ElfRelocation>(),
                size,
            )
        })
    }
}

impl Debug for Elf<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Elf")
            .field("data_len", &self.data.len())
            .field("header", &self.header)
            .finish()
    }
}
