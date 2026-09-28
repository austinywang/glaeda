//! Pure, bounded ELF64 dependency parsing for the reviewed Linux runtime closure.
//!
//! This module does not open a file, resolve a library, consult the loader cache, execute a
//! command, or construct runtime evidence. It only validates already-bounded ELF bytes and returns
//! the fixed loader kind plus canonical `DT_NEEDED` basenames for a later descriptor-bound R01
//! observer.

use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

use crate::personal_worker_runtime_contract::PersonalWorkerRuntimeArchitecture;

pub const LINUX_RUNTIME_ELF_MAX_BYTES: usize = 134_217_728;

const ELF_HEADER_BYTES: usize = 64;
const PROGRAM_HEADER_BYTES: usize = 56;
const MAX_PROGRAM_HEADERS: usize = 128;
const MAX_DYNAMIC_BYTES: usize = 1_048_576;
const MAX_STRING_TABLE_BYTES: usize = 4_194_304;
const MAX_NEEDED_LIBRARIES: usize = 256;
const MAX_INTERPRETER_BYTES: usize = 4_096;
const MAX_RUNPATH_BYTES: usize = 4_096;
const MAX_LIBRARY_NAME_BYTES: usize = 255;

const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
const EM_X86_64: u16 = 62;
const EM_AARCH64: u16 = 183;
const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const PT_GNU_STACK: u32 = 0x6474_e551;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const DT_NULL: i64 = 0;
const DT_NEEDED: i64 = 1;
const DT_STRTAB: i64 = 5;
const DT_STRSZ: i64 = 10;
const DT_RPATH: i64 = 15;
const DT_TEXTREL: i64 = 22;
const DT_FLAGS: i64 = 30;
const DT_RUNPATH: i64 = 29;
const DT_CONFIG: i64 = 0x6fff_fefa;
const DT_DEPAUDIT: i64 = 0x6fff_fefb;
const DT_AUDIT: i64 = 0x6fff_fefc;
const DT_FLAGS_1: i64 = 0x6fff_fffb;
const DT_AUXILIARY: i64 = 0x7fff_fffd;
const DT_FILTER: i64 = 0x7fff_ffff;
const DF_TEXTREL: u64 = 0x4;
const DF_1_NODEFLIB: u64 = 0x800;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxRuntimeElfLinkage {
    Static,
    Dynamic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxRuntimeDynamicLoader {
    Aarch64Gnu,
    X86_64Gnu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxRuntimeDynamicSearchPolicy {
    Default,
    SystemdPrivate,
}

impl LinuxRuntimeDynamicSearchPolicy {
    const fn expected_runpath(
        self,
        architecture: PersonalWorkerRuntimeArchitecture,
    ) -> Option<&'static str> {
        match (self, architecture) {
            (Self::Default, _) => None,
            (Self::SystemdPrivate, PersonalWorkerRuntimeArchitecture::Aarch64) => {
                Some("/usr/lib/aarch64-linux-gnu/systemd")
            }
            (Self::SystemdPrivate, PersonalWorkerRuntimeArchitecture::X86_64) => {
                Some("/usr/lib/x86_64-linux-gnu/systemd")
            }
        }
    }
}

impl LinuxRuntimeDynamicLoader {
    const fn expected_path(self) -> &'static str {
        match self {
            Self::Aarch64Gnu => "/lib/ld-linux-aarch64.so.1",
            Self::X86_64Gnu => "/lib64/ld-linux-x86-64.so.2",
        }
    }
}

/// Canonical external-runtime requirements parsed from one top-level Linux ELF.
///
/// This is not observation evidence: it contains no file identity, content digest, package
/// identity, resolved library, loader-cache/config, or revalidation proof.
#[derive(Clone, PartialEq, Eq)]
pub struct LinuxRuntimeElfDependency {
    architecture: PersonalWorkerRuntimeArchitecture,
    linkage: LinuxRuntimeElfLinkage,
    loader: Option<LinuxRuntimeDynamicLoader>,
    dynamic_search: Option<LinuxRuntimeDynamicSearchPolicy>,
    needed_libraries: Vec<String>,
}

/// Canonical shape parsed from one Linux dynamic-loader ELF object.
///
/// This is not observation evidence: it contains no path, file identity, content digest, package
/// identity, loader configuration, cache state, or revalidation proof.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct LinuxRuntimeLoaderObject {
    architecture: PersonalWorkerRuntimeArchitecture,
    loader: LinuxRuntimeDynamicLoader,
}

impl LinuxRuntimeLoaderObject {
    #[must_use]
    pub const fn architecture(self) -> PersonalWorkerRuntimeArchitecture {
        self.architecture
    }

    #[must_use]
    pub const fn loader(self) -> LinuxRuntimeDynamicLoader {
        self.loader
    }
}

impl fmt::Debug for LinuxRuntimeLoaderObject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinuxRuntimeLoaderObject")
            .field("architecture", &self.architecture)
            .field("loader", &self.loader)
            .finish()
    }
}

impl LinuxRuntimeElfDependency {
    #[must_use]
    pub const fn architecture(&self) -> PersonalWorkerRuntimeArchitecture {
        self.architecture
    }

    #[must_use]
    pub const fn linkage(&self) -> LinuxRuntimeElfLinkage {
        self.linkage
    }

    #[must_use]
    pub const fn loader(&self) -> Option<LinuxRuntimeDynamicLoader> {
        self.loader
    }

    #[must_use]
    pub const fn dynamic_search(&self) -> Option<LinuxRuntimeDynamicSearchPolicy> {
        self.dynamic_search
    }

    #[must_use]
    pub fn needed_libraries(&self) -> &[String] {
        &self.needed_libraries
    }
}

impl fmt::Debug for LinuxRuntimeElfDependency {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinuxRuntimeElfDependency")
            .field("architecture", &self.architecture)
            .field("linkage", &self.linkage)
            .field("loader", &self.loader)
            .field("dynamic_search", &self.dynamic_search)
            .field("needed_library_count", &self.needed_libraries.len())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxRuntimeElfErrorKind {
    Size,
    Format,
    Architecture,
    UnsafeRuntimeSearch,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct LinuxRuntimeElfError {
    pub kind: LinuxRuntimeElfErrorKind,
    pub code: &'static str,
    pub message: &'static str,
}

impl fmt::Debug for LinuxRuntimeElfError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinuxRuntimeElfError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for LinuxRuntimeElfError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for LinuxRuntimeElfError {}

/// Parse one bounded, little-endian ELF64 executable without resolving or executing anything.
///
/// # Errors
///
/// Rejects malformed or wrong-architecture bytes, executable stacks or writable-executable load
/// segments, alternate interpreters, `RPATH` or unreviewed `RUNPATH`, ambiguous virtual-address
/// mappings, and noncanonical dependency basenames.
pub fn parse_linux_runtime_elf_dependency(
    bytes: &[u8],
    expected_architecture: PersonalWorkerRuntimeArchitecture,
) -> Result<LinuxRuntimeElfDependency, LinuxRuntimeElfError> {
    let parsed = parse_runtime_elf(bytes, expected_architecture)?;
    let loader = match parsed.interpreter {
        Some(header) => Some(parse_interpreter(bytes, header, parsed.architecture)?),
        None => None,
    };
    let has_dynamic = parsed.dynamic.is_some();
    let dynamic_values = match parsed.dynamic {
        Some(header) => parse_dynamic(bytes, header, &parsed.loads)?,
        None => DynamicValues::default(),
    };
    if loader.is_some() != has_dynamic || (loader.is_none() && !dynamic_values.needed.is_empty()) {
        return Err(format_error());
    }

    let (linkage, needed_libraries) = if let Some(loader) = loader {
        let (needed_libraries, dynamic_search) =
            resolve_dynamic_strings(bytes, &parsed.loads, parsed.architecture, dynamic_values)?;
        return Ok(LinuxRuntimeElfDependency {
            architecture: parsed.architecture,
            linkage: LinuxRuntimeElfLinkage::Dynamic,
            loader: Some(loader),
            dynamic_search: Some(dynamic_search),
            needed_libraries,
        });
    } else {
        (LinuxRuntimeElfLinkage::Static, Vec::new())
    };
    Ok(LinuxRuntimeElfDependency {
        architecture: parsed.architecture,
        linkage,
        loader: None,
        dynamic_search: None,
        needed_libraries,
    })
}

/// Parse one bounded, little-endian ELF64 dynamic-loader object without resolving or executing it.
///
/// # Errors
///
/// Rejects malformed or wrong-architecture bytes, non-`ET_DYN` objects, executable stacks or
/// writable-executable load segments, an interpreter, external dependencies, alternate runtime
/// search authority, and ambiguous virtual-address mappings.
pub fn parse_linux_runtime_loader_object(
    bytes: &[u8],
    expected_architecture: PersonalWorkerRuntimeArchitecture,
) -> Result<LinuxRuntimeLoaderObject, LinuxRuntimeElfError> {
    let parsed = parse_runtime_elf(bytes, expected_architecture)?;
    if parsed.elf_type != ET_DYN || parsed.interpreter.is_some() || parsed.dynamic.is_none() {
        return Err(format_error());
    }
    let dynamic = parse_dynamic(
        bytes,
        parsed.dynamic.ok_or_else(format_error)?,
        &parsed.loads,
    )?;
    let (needed, dynamic_search) =
        resolve_dynamic_strings(bytes, &parsed.loads, parsed.architecture, dynamic)?;
    if !needed.is_empty() || dynamic_search != LinuxRuntimeDynamicSearchPolicy::Default {
        return Err(unsafe_search_error());
    }
    let loader = match parsed.architecture {
        PersonalWorkerRuntimeArchitecture::Aarch64 => LinuxRuntimeDynamicLoader::Aarch64Gnu,
        PersonalWorkerRuntimeArchitecture::X86_64 => LinuxRuntimeDynamicLoader::X86_64Gnu,
    };
    Ok(LinuxRuntimeLoaderObject {
        architecture: parsed.architecture,
        loader,
    })
}

struct ParsedRuntimeElf {
    elf_type: u16,
    architecture: PersonalWorkerRuntimeArchitecture,
    loads: Vec<ProgramHeader>,
    interpreter: Option<ProgramHeader>,
    dynamic: Option<ProgramHeader>,
}

fn parse_runtime_elf(
    bytes: &[u8],
    expected_architecture: PersonalWorkerRuntimeArchitecture,
) -> Result<ParsedRuntimeElf, LinuxRuntimeElfError> {
    if bytes.len() < ELF_HEADER_BYTES || bytes.len() > LINUX_RUNTIME_ELF_MAX_BYTES {
        return Err(size_error());
    }
    if &bytes[0..4] != b"\x7fELF"
        || bytes[4] != 2
        || bytes[5] != 1
        || bytes[6] != 1
        || !matches!(bytes[7], 0 | 3)
        || bytes[8] != 0
        || bytes[9..16].iter().any(|byte| *byte != 0)
    {
        return Err(format_error());
    }
    let elf_type = read_u16(bytes, 16)?;
    if !matches!(elf_type, ET_EXEC | ET_DYN) {
        return Err(format_error());
    }
    let machine = read_u16(bytes, 18)?;
    let architecture = match machine {
        EM_AARCH64 => PersonalWorkerRuntimeArchitecture::Aarch64,
        EM_X86_64 => PersonalWorkerRuntimeArchitecture::X86_64,
        _ => return Err(architecture_error()),
    };
    let entry_point = read_u64(bytes, 24)?;
    if architecture != expected_architecture
        || read_u32(bytes, 20)? != 1
        || entry_point == 0
        || read_u64(bytes, 32)? != ELF_HEADER_BYTES as u64
        || read_u32(bytes, 48)? != 0
        || read_u16(bytes, 52)? != ELF_HEADER_BYTES as u16
        || read_u16(bytes, 54)? != PROGRAM_HEADER_BYTES as u16
    {
        return Err(if architecture != expected_architecture {
            architecture_error()
        } else {
            format_error()
        });
    }
    let program_header_count = usize::from(read_u16(bytes, 56)?);
    if program_header_count == 0 || program_header_count > MAX_PROGRAM_HEADERS {
        return Err(format_error());
    }
    let program_table_bytes = program_header_count
        .checked_mul(PROGRAM_HEADER_BYTES)
        .and_then(|length| ELF_HEADER_BYTES.checked_add(length))
        .ok_or_else(size_error)?;
    if program_table_bytes > bytes.len() {
        return Err(size_error());
    }

    let mut loads = Vec::new();
    let mut interpreter = None;
    let mut dynamic = None;
    let mut gnu_stack = None;
    for index in 0..program_header_count {
        let offset = ELF_HEADER_BYTES + index * PROGRAM_HEADER_BYTES;
        let header = ProgramHeader::parse(bytes, offset)?;
        header.validate_file_range(bytes.len())?;
        match header.kind {
            PT_LOAD => {
                header.validate_load()?;
                loads.push(header);
            }
            PT_INTERP => {
                if interpreter.replace(header).is_some() {
                    return Err(format_error());
                }
            }
            PT_DYNAMIC => {
                if dynamic.replace(header).is_some() {
                    return Err(format_error());
                }
            }
            PT_GNU_STACK => {
                if header.flags & PF_X != 0 {
                    return Err(format_error());
                }
                gnu_stack = match gnu_stack {
                    Some(_) => return Err(format_error()),
                    None => Some(header),
                };
            }
            _ => {}
        }
    }
    if loads.is_empty() || gnu_stack.is_none() {
        return Err(format_error());
    }
    let entry_matches = loads
        .iter()
        .filter(|load| {
            load.flags & PF_X != 0
                && load
                    .virtual_address
                    .checked_add(load.memory_size)
                    .is_some_and(|end| entry_point >= load.virtual_address && entry_point < end)
        })
        .count();
    if entry_matches != 1 {
        return Err(format_error());
    }

    Ok(ParsedRuntimeElf {
        elf_type,
        architecture,
        loads,
        interpreter,
        dynamic,
    })
}

#[derive(Clone, Copy)]
struct ProgramHeader {
    kind: u32,
    flags: u32,
    offset: u64,
    virtual_address: u64,
    file_size: u64,
    memory_size: u64,
    alignment: u64,
}

impl ProgramHeader {
    fn parse(bytes: &[u8], offset: usize) -> Result<Self, LinuxRuntimeElfError> {
        Ok(Self {
            kind: read_u32(bytes, offset)?,
            flags: read_u32(bytes, offset + 4)?,
            offset: read_u64(bytes, offset + 8)?,
            virtual_address: read_u64(bytes, offset + 16)?,
            file_size: read_u64(bytes, offset + 32)?,
            memory_size: read_u64(bytes, offset + 40)?,
            alignment: read_u64(bytes, offset + 48)?,
        })
    }

    fn validate_file_range(self, file_length: usize) -> Result<(), LinuxRuntimeElfError> {
        if self.flags & !0b111 != 0 {
            return Err(format_error());
        }
        let end = self
            .offset
            .checked_add(self.file_size)
            .ok_or_else(size_error)?;
        if end > file_length as u64 {
            return Err(size_error());
        }
        Ok(())
    }

    fn validate_load(self) -> Result<(), LinuxRuntimeElfError> {
        if self.file_size > self.memory_size
            || self.flags & (PF_W | PF_X) == (PF_W | PF_X)
            || (!matches!(self.alignment, 0 | 1) && !self.alignment.is_power_of_two())
            || (self.alignment > 1
                && self.offset % self.alignment != self.virtual_address % self.alignment)
        {
            return Err(format_error());
        }
        Ok(())
    }

    fn file_slice(self, bytes: &[u8]) -> Result<&[u8], LinuxRuntimeElfError> {
        let start = usize::try_from(self.offset).map_err(|_| size_error())?;
        let length = usize::try_from(self.file_size).map_err(|_| size_error())?;
        let end = start.checked_add(length).ok_or_else(size_error)?;
        bytes.get(start..end).ok_or_else(size_error)
    }
}

#[derive(Default)]
struct DynamicValues {
    string_table_address: Option<u64>,
    string_table_size: Option<u64>,
    runpath: Option<u64>,
    needed: Vec<u64>,
}

fn parse_interpreter(
    bytes: &[u8],
    header: ProgramHeader,
    architecture: PersonalWorkerRuntimeArchitecture,
) -> Result<LinuxRuntimeDynamicLoader, LinuxRuntimeElfError> {
    let value = header.file_slice(bytes)?;
    if value.len() < 2 || value.len() > MAX_INTERPRETER_BYTES || value.last() != Some(&0) {
        return Err(format_error());
    }
    let path = &value[..value.len() - 1];
    if path.contains(&0) {
        return Err(format_error());
    }
    let path = std::str::from_utf8(path).map_err(|_| format_error())?;
    let loader = match architecture {
        PersonalWorkerRuntimeArchitecture::Aarch64 => LinuxRuntimeDynamicLoader::Aarch64Gnu,
        PersonalWorkerRuntimeArchitecture::X86_64 => LinuxRuntimeDynamicLoader::X86_64Gnu,
    };
    if path != loader.expected_path() {
        return Err(unsafe_search_error());
    }
    Ok(loader)
}

fn parse_dynamic(
    bytes: &[u8],
    header: ProgramHeader,
    loads: &[ProgramHeader],
) -> Result<DynamicValues, LinuxRuntimeElfError> {
    if header.file_size == 0
        || header.file_size > MAX_DYNAMIC_BYTES as u64
        || !header.file_size.is_multiple_of(16)
        || header.file_size > header.memory_size
    {
        return Err(format_error());
    }
    if mapped_file_offset(loads, header.virtual_address, header.file_size)? != header.offset {
        return Err(format_error());
    }
    let values = header.file_slice(bytes)?;
    let mut parsed = DynamicValues::default();
    let mut terminated = false;
    for entry in values.chunks_exact(16) {
        let tag = i64::from_le_bytes(entry[0..8].try_into().expect("exact dynamic tag"));
        let value = u64::from_le_bytes(entry[8..16].try_into().expect("exact dynamic value"));
        if terminated {
            if tag != DT_NULL || value != 0 {
                return Err(format_error());
            }
            continue;
        }
        match tag {
            DT_NULL => {
                if value != 0 {
                    return Err(format_error());
                }
                terminated = true;
            }
            DT_NEEDED => {
                if parsed.needed.len() == MAX_NEEDED_LIBRARIES {
                    return Err(size_error());
                }
                parsed.needed.push(value);
            }
            DT_STRTAB => set_once(&mut parsed.string_table_address, value)?,
            DT_STRSZ => set_once(&mut parsed.string_table_size, value)?,
            DT_RUNPATH => set_once(&mut parsed.runpath, value)?,
            DT_RPATH | DT_CONFIG | DT_DEPAUDIT | DT_AUDIT | DT_AUXILIARY | DT_FILTER => {
                return Err(unsafe_search_error());
            }
            DT_TEXTREL => return Err(format_error()),
            DT_FLAGS if value & DF_TEXTREL != 0 => return Err(format_error()),
            DT_FLAGS_1 if value & DF_1_NODEFLIB != 0 => return Err(unsafe_search_error()),
            _ => {}
        }
    }
    if !terminated {
        return Err(format_error());
    }
    if parsed.needed.is_empty() && parsed.runpath.is_none() {
        if parsed.string_table_address.is_some() != parsed.string_table_size.is_some() {
            return Err(format_error());
        }
    } else if parsed.string_table_address.is_none() || parsed.string_table_size.is_none() {
        return Err(format_error());
    }
    for load in loads {
        load.validate_load()?;
    }
    Ok(parsed)
}

fn resolve_dynamic_strings(
    bytes: &[u8],
    loads: &[ProgramHeader],
    architecture: PersonalWorkerRuntimeArchitecture,
    dynamic: DynamicValues,
) -> Result<(Vec<String>, LinuxRuntimeDynamicSearchPolicy), LinuxRuntimeElfError> {
    let (address, size) = match (dynamic.string_table_address, dynamic.string_table_size) {
        (Some(address), Some(size)) => (address, size),
        (None, None) if dynamic.needed.is_empty() && dynamic.runpath.is_none() => {
            return Ok((Vec::new(), LinuxRuntimeDynamicSearchPolicy::Default));
        }
        _ => return Err(format_error()),
    };
    if size == 0 || size > MAX_STRING_TABLE_BYTES as u64 {
        return Err(size_error());
    }
    let string_table = virtual_file_slice(bytes, loads, address, size)?;
    if string_table.first() != Some(&0) || string_table.last() != Some(&0) {
        return Err(format_error());
    }
    let mut seen = BTreeSet::new();
    let mut names = Vec::new();
    for needed_offset in dynamic.needed {
        let name = dynamic_string(string_table, needed_offset, MAX_LIBRARY_NAME_BYTES)?;
        if name.is_empty()
            || !name.iter().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-')
            })
        {
            return Err(unsafe_search_error());
        }
        let name = std::str::from_utf8(name).map_err(|_| format_error())?;
        if !seen.insert(name.to_owned()) {
            return Err(format_error());
        }
        names.push(name.to_owned());
    }
    let dynamic_search = match dynamic.runpath {
        None => LinuxRuntimeDynamicSearchPolicy::Default,
        Some(offset) => {
            let runpath = dynamic_string(string_table, offset, MAX_RUNPATH_BYTES)?;
            let runpath = std::str::from_utf8(runpath).map_err(|_| format_error())?;
            let policy = LinuxRuntimeDynamicSearchPolicy::SystemdPrivate;
            if policy.expected_runpath(architecture) != Some(runpath) {
                return Err(unsafe_search_error());
            }
            policy
        }
    };
    Ok((names, dynamic_search))
}

fn dynamic_string(
    string_table: &[u8],
    raw_offset: u64,
    maximum_length: usize,
) -> Result<&[u8], LinuxRuntimeElfError> {
    let offset = usize::try_from(raw_offset).map_err(|_| size_error())?;
    let tail = string_table.get(offset..).ok_or_else(format_error)?;
    if offset == 0 || string_table.get(offset - 1) != Some(&0) {
        return Err(format_error());
    }
    let length = tail
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(format_error)?;
    if length > maximum_length {
        return Err(size_error());
    }
    Ok(&tail[..length])
}

fn virtual_file_slice<'a>(
    bytes: &'a [u8],
    loads: &[ProgramHeader],
    address: u64,
    length: u64,
) -> Result<&'a [u8], LinuxRuntimeElfError> {
    let offset = mapped_file_offset(loads, address, length)?;
    let start = usize::try_from(offset).map_err(|_| size_error())?;
    let length = usize::try_from(length).map_err(|_| size_error())?;
    let end = start.checked_add(length).ok_or_else(size_error)?;
    bytes.get(start..end).ok_or_else(size_error)
}

fn mapped_file_offset(
    loads: &[ProgramHeader],
    address: u64,
    length: u64,
) -> Result<u64, LinuxRuntimeElfError> {
    let address_end = address.checked_add(length).ok_or_else(size_error)?;
    let matches = loads
        .iter()
        .copied()
        .filter_map(|load| {
            let file_virtual_end = load.virtual_address.checked_add(load.file_size)?;
            (address >= load.virtual_address && address_end <= file_virtual_end).then_some(load)
        })
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(format_error());
    }
    let load = matches[0];
    let relative = address - load.virtual_address;
    load.offset.checked_add(relative).ok_or_else(size_error)
}

fn set_once(slot: &mut Option<u64>, value: u64) -> Result<(), LinuxRuntimeElfError> {
    if slot.replace(value).is_some() {
        return Err(format_error());
    }
    Ok(())
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, LinuxRuntimeElfError> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(size_error)?
        .try_into()
        .expect("exact u16 bytes");
    Ok(u16::from_le_bytes(value))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, LinuxRuntimeElfError> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(size_error)?
        .try_into()
        .expect("exact u32 bytes");
    Ok(u32::from_le_bytes(value))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, LinuxRuntimeElfError> {
    let value = bytes
        .get(offset..offset + 8)
        .ok_or_else(size_error)?
        .try_into()
        .expect("exact u64 bytes");
    Ok(u64::from_le_bytes(value))
}

const fn error(
    kind: LinuxRuntimeElfErrorKind,
    code: &'static str,
    message: &'static str,
) -> LinuxRuntimeElfError {
    LinuxRuntimeElfError {
        kind,
        code,
        message,
    }
}

const fn size_error() -> LinuxRuntimeElfError {
    error(
        LinuxRuntimeElfErrorKind::Size,
        "runtime_elf_size",
        "Linux runtime ELF evidence exceeds its canonical bounds",
    )
}

const fn format_error() -> LinuxRuntimeElfError {
    error(
        LinuxRuntimeElfErrorKind::Format,
        "runtime_elf_format",
        "Linux runtime ELF evidence is malformed or noncanonical",
    )
}

const fn architecture_error() -> LinuxRuntimeElfError {
    error(
        LinuxRuntimeElfErrorKind::Architecture,
        "runtime_elf_architecture",
        "Linux runtime ELF architecture does not match",
    )
}

const fn unsafe_search_error() -> LinuxRuntimeElfError {
    error(
        LinuxRuntimeElfErrorKind::UnsafeRuntimeSearch,
        "runtime_elf_unsafe_search",
        "Linux runtime ELF selects an unreviewed runtime search path",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE_ADDRESS: u64 = 0x0040_0000;
    const INTERPRETER_OFFSET: usize = 0x0200;
    const DYNAMIC_OFFSET: usize = 0x0280;
    const STRING_TABLE_OFFSET: usize = 0x0380;

    #[test]
    fn dynamic_elf_returns_fixed_loader_and_ordered_needed_basenames() {
        let elf = dynamic_elf(
            PersonalWorkerRuntimeArchitecture::X86_64,
            &["libz.so.1", "libc.so.6"],
        );
        let parsed =
            parse_linux_runtime_elf_dependency(&elf, PersonalWorkerRuntimeArchitecture::X86_64)
                .expect("parse dynamic ELF");
        assert_eq!(
            parsed.architecture(),
            PersonalWorkerRuntimeArchitecture::X86_64
        );
        assert_eq!(parsed.linkage(), LinuxRuntimeElfLinkage::Dynamic);
        assert_eq!(parsed.loader(), Some(LinuxRuntimeDynamicLoader::X86_64Gnu));
        assert_eq!(
            parsed.dynamic_search(),
            Some(LinuxRuntimeDynamicSearchPolicy::Default)
        );
        assert_eq!(
            parsed.needed_libraries(),
            &["libz.so.1".to_owned(), "libc.so.6".to_owned()]
        );
        let debug = format!("{parsed:?}");
        assert!(debug.contains("needed_library_count: 2"));
        assert!(!debug.contains("libc.so.6"));
    }

    #[test]
    fn static_elf_has_no_loader_or_external_dependencies() {
        let elf = static_elf(PersonalWorkerRuntimeArchitecture::Aarch64);
        let parsed =
            parse_linux_runtime_elf_dependency(&elf, PersonalWorkerRuntimeArchitecture::Aarch64)
                .expect("parse static ELF");
        assert_eq!(parsed.linkage(), LinuxRuntimeElfLinkage::Static);
        assert_eq!(parsed.loader(), None);
        assert_eq!(parsed.dynamic_search(), None);
        assert!(parsed.needed_libraries().is_empty());
    }

    #[test]
    fn dynamic_loader_object_has_exact_architecture_and_no_external_authority() {
        for architecture in [
            PersonalWorkerRuntimeArchitecture::Aarch64,
            PersonalWorkerRuntimeArchitecture::X86_64,
        ] {
            let elf = loader_elf(architecture, &[]);
            let parsed = parse_linux_runtime_loader_object(&elf, architecture)
                .expect("parse dynamic-loader object");
            assert_eq!(parsed.architecture(), architecture);
            assert_eq!(
                parsed.loader(),
                match architecture {
                    PersonalWorkerRuntimeArchitecture::Aarch64 => {
                        LinuxRuntimeDynamicLoader::Aarch64Gnu
                    }
                    PersonalWorkerRuntimeArchitecture::X86_64 => {
                        LinuxRuntimeDynamicLoader::X86_64Gnu
                    }
                }
            );
            let debug = format!("{parsed:?}");
            assert!(!debug.contains("/lib"));
            assert!(!debug.contains("0x"));
        }
    }

    #[test]
    fn loader_object_refuses_wrong_architecture_type_interpreter_and_dependencies() {
        let architecture = PersonalWorkerRuntimeArchitecture::X86_64;
        let elf = loader_elf(architecture, &[]);
        assert_eq!(
            parse_linux_runtime_loader_object(&elf, PersonalWorkerRuntimeArchitecture::Aarch64)
                .expect_err("wrong loader architecture")
                .kind,
            LinuxRuntimeElfErrorKind::Architecture
        );

        let mut executable = elf.clone();
        write_u16(&mut executable, 16, ET_EXEC);
        assert_eq!(
            parse_linux_runtime_loader_object(&executable, architecture)
                .expect_err("loader must be ET_DYN")
                .kind,
            LinuxRuntimeElfErrorKind::Format
        );

        let mut interpreted = elf;
        write_u16(&mut interpreted, 56, 4);
        let interpreter = b"/lib64/ld-linux-x86-64.so.2\0";
        interpreted[INTERPRETER_OFFSET..INTERPRETER_OFFSET + interpreter.len()]
            .copy_from_slice(interpreter);
        write_program_header(
            &mut interpreted,
            3,
            PT_INTERP,
            4,
            INTERPRETER_OFFSET as u64,
            BASE_ADDRESS + INTERPRETER_OFFSET as u64,
            interpreter.len() as u64,
            interpreter.len() as u64,
            1,
        );
        assert_eq!(
            parse_linux_runtime_loader_object(&interpreted, architecture)
                .expect_err("loader must not select another interpreter")
                .kind,
            LinuxRuntimeElfErrorKind::Format
        );

        let dependency = loader_elf(architecture, &["libc.so.6"]);
        assert_eq!(
            parse_linux_runtime_loader_object(&dependency, architecture)
                .expect_err("loader dependency")
                .kind,
            LinuxRuntimeElfErrorKind::UnsafeRuntimeSearch
        );
    }

    #[test]
    fn loader_object_refuses_runtime_search_and_text_relocations() {
        let architecture = PersonalWorkerRuntimeArchitecture::X86_64;
        for tag in [
            DT_RPATH,
            DT_CONFIG,
            DT_DEPAUDIT,
            DT_AUDIT,
            DT_AUXILIARY,
            DT_FILTER,
        ] {
            let mut elf = loader_elf(architecture, &[]);
            write_dynamic(&mut elf, 0, tag, 0);
            assert_eq!(
                parse_linux_runtime_loader_object(&elf, architecture)
                    .expect_err("loader search authority")
                    .kind,
                LinuxRuntimeElfErrorKind::UnsafeRuntimeSearch
            );
        }
        for (tag, value, expected_kind) in [
            (DT_TEXTREL, 0, LinuxRuntimeElfErrorKind::Format),
            (DT_FLAGS, DF_TEXTREL, LinuxRuntimeElfErrorKind::Format),
            (
                DT_FLAGS_1,
                DF_1_NODEFLIB,
                LinuxRuntimeElfErrorKind::UnsafeRuntimeSearch,
            ),
        ] {
            let mut elf = loader_elf(architecture, &[]);
            write_dynamic(&mut elf, 0, tag, value);
            assert_eq!(
                parse_linux_runtime_loader_object(&elf, architecture)
                    .expect_err("loader relocation or search authority")
                    .kind,
                expected_kind
            );
        }

        let mut runpath = loader_elf(architecture, &[]);
        add_loader_runpath(&mut runpath, "/usr/lib/x86_64-linux-gnu/systemd");
        assert_eq!(
            parse_linux_runtime_loader_object(&runpath, architecture)
                .expect_err("loader runpath")
                .kind,
            LinuxRuntimeElfErrorKind::UnsafeRuntimeSearch
        );
    }

    #[test]
    fn wrong_architecture_and_alternate_interpreter_fail_closed() {
        let mut elf = dynamic_elf(PersonalWorkerRuntimeArchitecture::Aarch64, &["libc.so.6"]);
        assert_eq!(
            parse_linux_runtime_elf_dependency(&elf, PersonalWorkerRuntimeArchitecture::X86_64)
                .expect_err("wrong architecture")
                .kind,
            LinuxRuntimeElfErrorKind::Architecture
        );
        let interpreter = b"/tmp/ld-linux-aarch64.so.1\0";
        elf[INTERPRETER_OFFSET..INTERPRETER_OFFSET + interpreter.len()]
            .copy_from_slice(interpreter);
        write_u64(
            &mut elf,
            ELF_HEADER_BYTES + PROGRAM_HEADER_BYTES + 32,
            interpreter.len() as u64,
        );
        assert_eq!(
            parse_linux_runtime_elf_dependency(&elf, PersonalWorkerRuntimeArchitecture::Aarch64)
                .expect_err("alternate interpreter")
                .kind,
            LinuxRuntimeElfErrorKind::UnsafeRuntimeSearch
        );
    }

    #[test]
    fn unsafe_interpreter_precedes_malformed_dynamic_metadata() {
        let architecture = PersonalWorkerRuntimeArchitecture::X86_64;
        let mut elf = dynamic_elf(architecture, &["libc.so.6"]);
        let interpreter = b"/tmp/ld-linux-x86-64.so.2\0";
        elf[INTERPRETER_OFFSET..INTERPRETER_OFFSET + interpreter.len()]
            .copy_from_slice(interpreter);
        write_u64(
            &mut elf,
            ELF_HEADER_BYTES + PROGRAM_HEADER_BYTES + 32,
            interpreter.len() as u64,
        );
        write_dynamic(&mut elf, 0, DT_TEXTREL, 0);
        assert_eq!(
            parse_linux_runtime_elf_dependency(&elf, architecture)
                .expect_err("unsafe interpreter precedes invalid dynamic metadata")
                .kind,
            LinuxRuntimeElfErrorKind::UnsafeRuntimeSearch
        );
    }

    #[test]
    fn alternate_search_authority_and_dependency_paths_are_rejected() {
        for tag in [
            DT_RPATH,
            DT_CONFIG,
            DT_DEPAUDIT,
            DT_AUDIT,
            DT_AUXILIARY,
            DT_FILTER,
        ] {
            let mut elf = dynamic_elf(PersonalWorkerRuntimeArchitecture::X86_64, &["libc.so.6"]);
            write_i64(&mut elf, DYNAMIC_OFFSET, tag);
            assert_eq!(
                parse_linux_runtime_elf_dependency(&elf, PersonalWorkerRuntimeArchitecture::X86_64)
                    .expect_err("runtime search tag")
                    .kind,
                LinuxRuntimeElfErrorKind::UnsafeRuntimeSearch
            );
        }

        let mut alternate_runpath =
            dynamic_elf(PersonalWorkerRuntimeArchitecture::X86_64, &["libc.so.6"]);
        add_runpath(&mut alternate_runpath, 1, "/tmp/systemd");
        assert_eq!(
            parse_linux_runtime_elf_dependency(
                &alternate_runpath,
                PersonalWorkerRuntimeArchitecture::X86_64
            )
            .expect_err("alternate runpath")
            .kind,
            LinuxRuntimeElfErrorKind::UnsafeRuntimeSearch
        );

        let elf = dynamic_elf(PersonalWorkerRuntimeArchitecture::X86_64, &["../libc.so.6"]);
        assert_eq!(
            parse_linux_runtime_elf_dependency(&elf, PersonalWorkerRuntimeArchitecture::X86_64)
                .expect_err("dependency path")
                .kind,
            LinuxRuntimeElfErrorKind::UnsafeRuntimeSearch
        );

        for (tag, value, expected_kind) in [
            (DT_TEXTREL, 0, LinuxRuntimeElfErrorKind::Format),
            (DT_FLAGS, DF_TEXTREL, LinuxRuntimeElfErrorKind::Format),
            (
                DT_FLAGS_1,
                DF_1_NODEFLIB,
                LinuxRuntimeElfErrorKind::UnsafeRuntimeSearch,
            ),
        ] {
            let mut elf = dynamic_elf(PersonalWorkerRuntimeArchitecture::X86_64, &["libc.so.6"]);
            write_dynamic(&mut elf, 0, tag, value);
            assert_eq!(
                parse_linux_runtime_elf_dependency(&elf, PersonalWorkerRuntimeArchitecture::X86_64)
                    .expect_err("alternate loader authority")
                    .kind,
                expected_kind
            );
        }
    }

    #[test]
    fn exact_architecture_specific_systemd_runpath_is_typed() {
        for (architecture, runpath) in [
            (
                PersonalWorkerRuntimeArchitecture::Aarch64,
                "/usr/lib/aarch64-linux-gnu/systemd",
            ),
            (
                PersonalWorkerRuntimeArchitecture::X86_64,
                "/usr/lib/x86_64-linux-gnu/systemd",
            ),
        ] {
            let mut elf = dynamic_elf(architecture, &["libc.so.6"]);
            add_runpath(&mut elf, 1, runpath);
            let parsed = parse_linux_runtime_elf_dependency(&elf, architecture)
                .expect("exact systemd runpath");
            assert_eq!(
                parsed.dynamic_search(),
                Some(LinuxRuntimeDynamicSearchPolicy::SystemdPrivate)
            );
        }
    }

    #[test]
    fn duplicate_needed_or_ambiguous_string_mapping_is_rejected() {
        let elf = dynamic_elf(
            PersonalWorkerRuntimeArchitecture::X86_64,
            &["libc.so.6", "libc.so.6"],
        );
        assert_eq!(
            parse_linux_runtime_elf_dependency(&elf, PersonalWorkerRuntimeArchitecture::X86_64)
                .expect_err("duplicate needed")
                .kind,
            LinuxRuntimeElfErrorKind::Format
        );

        let mut interior_alias =
            dynamic_elf(PersonalWorkerRuntimeArchitecture::X86_64, &["libc.so.6"]);
        write_dynamic(&mut interior_alias, 0, DT_NEEDED, 2);
        assert_eq!(
            parse_linux_runtime_elf_dependency(
                &interior_alias,
                PersonalWorkerRuntimeArchitecture::X86_64
            )
            .expect_err("interior string-table alias")
            .kind,
            LinuxRuntimeElfErrorKind::Format
        );

        let mut elf = dynamic_elf(PersonalWorkerRuntimeArchitecture::X86_64, &["libc.so.6"]);
        let elf_length = elf.len() as u64;
        write_u16(&mut elf, 56, 5);
        write_program_header(
            &mut elf,
            4,
            PT_LOAD,
            4,
            0,
            BASE_ADDRESS,
            elf_length,
            elf_length,
            0x1000,
        );
        assert_eq!(
            parse_linux_runtime_elf_dependency(&elf, PersonalWorkerRuntimeArchitecture::X86_64)
                .expect_err("ambiguous string mapping")
                .kind,
            LinuxRuntimeElfErrorKind::Format
        );
    }

    #[test]
    fn executable_stack_writable_executable_load_and_bounds_are_rejected() {
        let mut executable_stack = static_elf(PersonalWorkerRuntimeArchitecture::X86_64);
        write_u32(
            &mut executable_stack,
            ELF_HEADER_BYTES + PROGRAM_HEADER_BYTES + 4,
            PF_W | PF_X,
        );
        assert!(
            parse_linux_runtime_elf_dependency(
                &executable_stack,
                PersonalWorkerRuntimeArchitecture::X86_64
            )
            .is_err()
        );

        let mut writable_executable = static_elf(PersonalWorkerRuntimeArchitecture::X86_64);
        write_u32(&mut writable_executable, ELF_HEADER_BYTES + 4, PF_W | PF_X);
        assert!(
            parse_linux_runtime_elf_dependency(
                &writable_executable,
                PersonalWorkerRuntimeArchitecture::X86_64
            )
            .is_err()
        );

        let mut truncated = dynamic_elf(PersonalWorkerRuntimeArchitecture::X86_64, &["libc.so.6"]);
        truncated.truncate(DYNAMIC_OFFSET + 8);
        assert_eq!(
            parse_linux_runtime_elf_dependency(
                &truncated,
                PersonalWorkerRuntimeArchitecture::X86_64
            )
            .expect_err("truncated dynamic segment")
            .kind,
            LinuxRuntimeElfErrorKind::Size
        );

        let mut unmapped_entry = static_elf(PersonalWorkerRuntimeArchitecture::X86_64);
        write_u64(&mut unmapped_entry, 24, BASE_ADDRESS + 0x10_0000);
        assert_eq!(
            parse_linux_runtime_elf_dependency(
                &unmapped_entry,
                PersonalWorkerRuntimeArchitecture::X86_64
            )
            .expect_err("entry outside executable load")
            .kind,
            LinuxRuntimeElfErrorKind::Format
        );

        let mut rebound_dynamic =
            dynamic_elf(PersonalWorkerRuntimeArchitecture::X86_64, &["libc.so.6"]);
        write_u64(
            &mut rebound_dynamic,
            ELF_HEADER_BYTES + 2 * PROGRAM_HEADER_BYTES + 16,
            BASE_ADDRESS + DYNAMIC_OFFSET as u64 + 1,
        );
        assert_eq!(
            parse_linux_runtime_elf_dependency(
                &rebound_dynamic,
                PersonalWorkerRuntimeArchitecture::X86_64
            )
            .expect_err("dynamic address and file range disagree")
            .kind,
            LinuxRuntimeElfErrorKind::Format
        );
    }

    #[test]
    fn disposable_noble_packages_use_only_the_admitted_elf_shapes() {
        if std::env::var("GLAEDA_DISPOSABLE_PROBE").as_deref() != Ok("1") {
            return;
        }
        let architecture = match std::env::consts::ARCH {
            "aarch64" => PersonalWorkerRuntimeArchitecture::Aarch64,
            "x86_64" => PersonalWorkerRuntimeArchitecture::X86_64,
            other => panic!("unsupported package-probe architecture: {other}"),
        };
        for path in [
            "/usr/bin/podman",
            "/usr/bin/git",
            "/usr/sbin/runuser",
            "/usr/bin/env",
            "/usr/bin/systemctl",
            "/usr/bin/systemd-run",
            "/usr/bin/crun",
            "/usr/bin/conmon",
            "/usr/bin/catatonit",
            "/usr/bin/newuidmap",
            "/usr/bin/newgidmap",
        ] {
            let metadata = std::fs::metadata(path)
                .unwrap_or_else(|error| panic!("read metadata for {path}: {error}"));
            assert!(
                metadata.len() <= LINUX_RUNTIME_ELF_MAX_BYTES as u64,
                "package ELF exceeds parser bound: {path}"
            );
            let bytes = std::fs::read(path)
                .unwrap_or_else(|error| panic!("read package ELF {path}: {error}"));
            let parsed = parse_linux_runtime_elf_dependency(&bytes, architecture)
                .unwrap_or_else(|error| panic!("parse package ELF {path}: {error}"));
            let selects_systemd_private = matches!(
                parsed.dynamic_search(),
                Some(LinuxRuntimeDynamicSearchPolicy::SystemdPrivate)
            );
            assert_eq!(
                selects_systemd_private,
                matches!(path, "/usr/bin/systemctl" | "/usr/bin/systemd-run"),
                "unexpected systemd-private search policy: {path}"
            );
        }
    }

    #[test]
    fn disposable_noble_dynamic_loader_has_the_admitted_object_shape() {
        if std::env::var("GLAEDA_DISPOSABLE_PROBE").as_deref() != Ok("1") {
            return;
        }
        let (architecture, path) = match std::env::consts::ARCH {
            "aarch64" => (
                PersonalWorkerRuntimeArchitecture::Aarch64,
                LinuxRuntimeDynamicLoader::Aarch64Gnu.expected_path(),
            ),
            "x86_64" => (
                PersonalWorkerRuntimeArchitecture::X86_64,
                LinuxRuntimeDynamicLoader::X86_64Gnu.expected_path(),
            ),
            other => panic!("unsupported package-probe architecture: {other}"),
        };
        let metadata = std::fs::metadata(path)
            .unwrap_or_else(|error| panic!("read loader metadata for {path}: {error}"));
        assert!(metadata.len() <= LINUX_RUNTIME_ELF_MAX_BYTES as u64);
        let bytes = std::fs::read(path)
            .unwrap_or_else(|error| panic!("read loader object {path}: {error}"));
        let parsed = parse_linux_runtime_loader_object(&bytes, architecture)
            .unwrap_or_else(|error| panic!("parse loader object {path}: {error}"));
        assert_eq!(parsed.architecture(), architecture);
    }

    fn static_elf(architecture: PersonalWorkerRuntimeArchitecture) -> Vec<u8> {
        let mut bytes = vec![0; 0x0400];
        write_header(&mut bytes, architecture, 2);
        let length = bytes.len() as u64;
        write_program_header(
            &mut bytes,
            0,
            PT_LOAD,
            5,
            0,
            BASE_ADDRESS,
            length,
            length,
            0x1000,
        );
        write_program_header(&mut bytes, 1, PT_GNU_STACK, PF_W, 0, 0, 0, 0, 16);
        bytes
    }

    fn dynamic_elf(architecture: PersonalWorkerRuntimeArchitecture, needed: &[&str]) -> Vec<u8> {
        let mut bytes = vec![0; 0x0800];
        write_header(&mut bytes, architecture, 4);
        let loader = match architecture {
            PersonalWorkerRuntimeArchitecture::Aarch64 => LinuxRuntimeDynamicLoader::Aarch64Gnu,
            PersonalWorkerRuntimeArchitecture::X86_64 => LinuxRuntimeDynamicLoader::X86_64Gnu,
        };
        let interpreter = format!("{}\0", loader.expected_path());
        bytes[INTERPRETER_OFFSET..INTERPRETER_OFFSET + interpreter.len()]
            .copy_from_slice(interpreter.as_bytes());

        let mut string_table = vec![0];
        let mut needed_offsets = Vec::new();
        for name in needed {
            needed_offsets.push(string_table.len() as u64);
            string_table.extend_from_slice(name.as_bytes());
            string_table.push(0);
        }
        bytes[STRING_TABLE_OFFSET..STRING_TABLE_OFFSET + string_table.len()]
            .copy_from_slice(&string_table);

        let dynamic_entries = needed.len() + 3;
        let dynamic_size = dynamic_entries * 16;
        for (index, offset) in needed_offsets.into_iter().enumerate() {
            write_dynamic(&mut bytes, index, DT_NEEDED, offset);
        }
        write_dynamic(
            &mut bytes,
            needed.len(),
            DT_STRTAB,
            BASE_ADDRESS + STRING_TABLE_OFFSET as u64,
        );
        write_dynamic(
            &mut bytes,
            needed.len() + 1,
            DT_STRSZ,
            string_table.len() as u64,
        );
        write_dynamic(&mut bytes, needed.len() + 2, DT_NULL, 0);

        let length = bytes.len() as u64;
        write_program_header(
            &mut bytes,
            0,
            PT_LOAD,
            5,
            0,
            BASE_ADDRESS,
            length,
            length,
            0x1000,
        );
        write_program_header(
            &mut bytes,
            1,
            PT_INTERP,
            4,
            INTERPRETER_OFFSET as u64,
            BASE_ADDRESS + INTERPRETER_OFFSET as u64,
            interpreter.len() as u64,
            interpreter.len() as u64,
            1,
        );
        write_program_header(
            &mut bytes,
            2,
            PT_DYNAMIC,
            PF_W,
            DYNAMIC_OFFSET as u64,
            BASE_ADDRESS + DYNAMIC_OFFSET as u64,
            dynamic_size as u64,
            dynamic_size as u64,
            8,
        );
        write_program_header(&mut bytes, 3, PT_GNU_STACK, PF_W, 0, 0, 0, 0, 16);
        bytes
    }

    fn loader_elf(architecture: PersonalWorkerRuntimeArchitecture, needed: &[&str]) -> Vec<u8> {
        let mut bytes = vec![0; 0x0800];
        write_header(&mut bytes, architecture, 3);

        let mut string_table = vec![0];
        let mut needed_offsets = Vec::new();
        for name in needed {
            needed_offsets.push(string_table.len() as u64);
            string_table.extend_from_slice(name.as_bytes());
            string_table.push(0);
        }
        bytes[STRING_TABLE_OFFSET..STRING_TABLE_OFFSET + string_table.len()]
            .copy_from_slice(&string_table);

        for (index, offset) in needed_offsets.into_iter().enumerate() {
            write_dynamic(&mut bytes, index, DT_NEEDED, offset);
        }
        write_dynamic(
            &mut bytes,
            needed.len(),
            DT_STRTAB,
            BASE_ADDRESS + STRING_TABLE_OFFSET as u64,
        );
        write_dynamic(
            &mut bytes,
            needed.len() + 1,
            DT_STRSZ,
            string_table.len() as u64,
        );
        write_dynamic(&mut bytes, needed.len() + 2, DT_NULL, 0);
        let dynamic_size = (needed.len() + 3) as u64 * 16;

        let length = bytes.len() as u64;
        write_program_header(
            &mut bytes,
            0,
            PT_LOAD,
            5,
            0,
            BASE_ADDRESS,
            length,
            length,
            0x1000,
        );
        write_program_header(
            &mut bytes,
            1,
            PT_DYNAMIC,
            PF_W,
            DYNAMIC_OFFSET as u64,
            BASE_ADDRESS + DYNAMIC_OFFSET as u64,
            dynamic_size,
            dynamic_size,
            8,
        );
        write_program_header(&mut bytes, 2, PT_GNU_STACK, PF_W, 0, 0, 0, 0, 16);
        bytes
    }

    fn add_loader_runpath(bytes: &mut [u8], runpath: &str) {
        bytes[STRING_TABLE_OFFSET + 1..STRING_TABLE_OFFSET + 1 + runpath.len()]
            .copy_from_slice(runpath.as_bytes());
        bytes[STRING_TABLE_OFFSET + 1 + runpath.len()] = 0;
        write_dynamic(bytes, 1, DT_STRSZ, runpath.len() as u64 + 2);
        write_dynamic(bytes, 2, DT_RUNPATH, 1);
        write_dynamic(bytes, 3, DT_NULL, 0);
        let dynamic_size = 4_u64 * 16;
        write_u64(
            bytes,
            ELF_HEADER_BYTES + PROGRAM_HEADER_BYTES + 32,
            dynamic_size,
        );
        write_u64(
            bytes,
            ELF_HEADER_BYTES + PROGRAM_HEADER_BYTES + 40,
            dynamic_size,
        );
    }

    fn add_runpath(bytes: &mut [u8], needed_count: usize, runpath: &str) {
        let string_size_entry = DYNAMIC_OFFSET + (needed_count + 1) * 16;
        let old_string_size = read_u64(bytes, string_size_entry + 8).expect("string-table size");
        let runpath_offset = old_string_size;
        let runpath_start = STRING_TABLE_OFFSET + old_string_size as usize;
        bytes[runpath_start..runpath_start + runpath.len()].copy_from_slice(runpath.as_bytes());
        bytes[runpath_start + runpath.len()] = 0;
        write_u64(
            bytes,
            string_size_entry + 8,
            old_string_size + runpath.len() as u64 + 1,
        );
        write_dynamic(bytes, needed_count + 2, DT_RUNPATH, runpath_offset);
        write_dynamic(bytes, needed_count + 3, DT_NULL, 0);
        let dynamic_size = (needed_count + 4) as u64 * 16;
        write_u64(
            bytes,
            ELF_HEADER_BYTES + 2 * PROGRAM_HEADER_BYTES + 32,
            dynamic_size,
        );
        write_u64(
            bytes,
            ELF_HEADER_BYTES + 2 * PROGRAM_HEADER_BYTES + 40,
            dynamic_size,
        );
    }

    fn write_header(
        bytes: &mut [u8],
        architecture: PersonalWorkerRuntimeArchitecture,
        program_headers: u16,
    ) {
        bytes[0..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[6] = 1;
        write_u16(bytes, 16, ET_DYN);
        write_u16(
            bytes,
            18,
            match architecture {
                PersonalWorkerRuntimeArchitecture::Aarch64 => EM_AARCH64,
                PersonalWorkerRuntimeArchitecture::X86_64 => EM_X86_64,
            },
        );
        write_u32(bytes, 20, 1);
        write_u64(bytes, 24, BASE_ADDRESS + 0x0100);
        write_u64(bytes, 32, ELF_HEADER_BYTES as u64);
        write_u16(bytes, 52, ELF_HEADER_BYTES as u16);
        write_u16(bytes, 54, PROGRAM_HEADER_BYTES as u16);
        write_u16(bytes, 56, program_headers);
    }

    #[allow(clippy::too_many_arguments)]
    fn write_program_header(
        bytes: &mut [u8],
        index: usize,
        kind: u32,
        flags: u32,
        file_offset: u64,
        virtual_address: u64,
        file_size: u64,
        memory_size: u64,
        alignment: u64,
    ) {
        let offset = ELF_HEADER_BYTES + index * PROGRAM_HEADER_BYTES;
        write_u32(bytes, offset, kind);
        write_u32(bytes, offset + 4, flags);
        write_u64(bytes, offset + 8, file_offset);
        write_u64(bytes, offset + 16, virtual_address);
        write_u64(bytes, offset + 32, file_size);
        write_u64(bytes, offset + 40, memory_size);
        write_u64(bytes, offset + 48, alignment);
    }

    fn write_dynamic(bytes: &mut [u8], index: usize, tag: i64, value: u64) {
        let offset = DYNAMIC_OFFSET + index * 16;
        write_i64(bytes, offset, tag);
        write_u64(bytes, offset + 8, value);
    }

    fn write_i64(bytes: &mut [u8], offset: usize, value: i64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
}
