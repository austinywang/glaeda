//! Pure, bounded parsing of the admitted glibc 2.39 `ld.so.cache` format.
//!
//! This module does not open `/etc/ld.so.cache`, resolve or open a library, inspect a search
//! directory, execute `ldconfig`/`ldd`, or construct runtime evidence. It only validates
//! already-bounded bytes from the current little-endian glibc 1.1 cache format for a later
//! descriptor-bound R01 observer. It can also reproduce cache selection from an explicitly
//! supplied, caller-owned capability profile, but that profile is not host evidence and the
//! selected path remains untrusted.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

use crate::personal_worker_runtime_contract::PersonalWorkerRuntimeArchitecture;

pub const LINUX_DYNAMIC_LOADER_CACHE_MAX_BYTES: usize = 16_777_216;

const HEADER_BYTES: usize = 48;
const ENTRY_BYTES: usize = 24;
const MAX_ENTRIES: usize = 65_536;
const MAX_STRING_TABLE_BYTES: usize = 8_388_608;
const MAX_LIBRARY_NAME_BYTES: usize = 255;
const MAX_LIBRARY_PATH_BYTES: usize = 4_096;
const MAX_HWCAP_NAMES: usize = 256;
const MAX_HWCAP_NAME_BYTES: usize = 255;
const MAX_GENERATOR_BYTES: usize = 1_024;
const CACHE_MAGIC: &[u8; 17] = b"glibc-ld.so.cache";
const CACHE_VERSION: &[u8; 3] = b"1.1";
const LITTLE_ENDIAN_FLAG: u8 = 2;
const GENERIC_ELF_LIBC6_CACHE_ID: i32 = 0x0003;
const X86_64_CACHE_ID: i32 = 0x0303;
const AARCH64_CACHE_ID: i32 = 0x0a03;
const HWCAP_EXTENSION_BIT: u64 = 1_u64 << 62;
const HWCAP_ISA_MASK: u64 = 0x03ff;
const EXTENSION_HEADER_BYTES: usize = 8;
const EXTENSION_SECTION_BYTES: usize = 16;
const EXTENSION_MAGIC: u32 = (-358_342_284_i32) as u32;
const EXTENSION_TAG_GENERATOR: u32 = 0;
const EXTENSION_TAG_GLIBC_HWCAPS: u32 = 1;

#[derive(Clone, PartialEq, Eq)]
pub struct LinuxDynamicLoaderCacheEntry {
    library_name: String,
    library_path: String,
    hwcap_name: Option<String>,
    isa_level: u16,
}

impl LinuxDynamicLoaderCacheEntry {
    #[must_use]
    pub fn library_name(&self) -> &str {
        &self.library_name
    }

    #[must_use]
    pub fn library_path(&self) -> &str {
        &self.library_path
    }

    #[must_use]
    pub fn hwcap_name(&self) -> Option<&str> {
        self.hwcap_name.as_deref()
    }

    #[must_use]
    pub const fn isa_level(&self) -> u16 {
        self.isa_level
    }
}

impl fmt::Debug for LinuxDynamicLoaderCacheEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinuxDynamicLoaderCacheEntry")
            .field("has_glibc_hwcap", &self.hwcap_name.is_some())
            .field("isa_level", &self.isa_level)
            .finish_non_exhaustive()
    }
}

/// Canonical semantics parsed from one complete current-format loader cache.
///
/// This is not observation evidence: it carries no cache path identity, owner, mode, content
/// digest, library file identity, search-directory proof, or revalidation receipt.
#[derive(Clone, PartialEq, Eq)]
pub struct LinuxDynamicLoaderCache {
    architecture: PersonalWorkerRuntimeArchitecture,
    entries: Vec<LinuxDynamicLoaderCacheEntry>,
    ignored_incompatible_entry_count: usize,
    glibc_hwcap_names: Vec<String>,
}

impl LinuxDynamicLoaderCache {
    #[must_use]
    pub const fn architecture(&self) -> PersonalWorkerRuntimeArchitecture {
        self.architecture
    }

    #[must_use]
    pub fn entries(&self) -> &[LinuxDynamicLoaderCacheEntry] {
        &self.entries
    }

    /// Number of fully validated cache entries that the admitted loader ignores by architecture.
    #[must_use]
    pub const fn ignored_incompatible_entry_count(&self) -> usize {
        self.ignored_incompatible_entry_count
    }

    #[must_use]
    pub fn glibc_hwcap_names(&self) -> &[String] {
        &self.glibc_hwcap_names
    }
}

impl fmt::Debug for LinuxDynamicLoaderCache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinuxDynamicLoaderCache")
            .field("architecture", &self.architecture)
            .field("entry_count", &self.entries.len())
            .field(
                "ignored_incompatible_entry_count",
                &self.ignored_incompatible_entry_count,
            )
            .field("glibc_hwcap_name_count", &self.glibc_hwcap_names.len())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxDynamicLoaderCapabilityProfile {
    Aarch64Baseline,
    X86_64Baseline,
    X86_64V2,
    X86_64V3,
    X86_64V4,
}

impl LinuxDynamicLoaderCapabilityProfile {
    #[must_use]
    pub const fn architecture(self) -> PersonalWorkerRuntimeArchitecture {
        match self {
            Self::Aarch64Baseline => PersonalWorkerRuntimeArchitecture::Aarch64,
            Self::X86_64Baseline | Self::X86_64V2 | Self::X86_64V3 | Self::X86_64V4 => {
                PersonalWorkerRuntimeArchitecture::X86_64
            }
        }
    }

    const fn supports_isa_level(self, level: u16) -> bool {
        let maximum = match self {
            Self::Aarch64Baseline | Self::X86_64Baseline => 0,
            Self::X86_64V2 => 1,
            Self::X86_64V3 => 2,
            Self::X86_64V4 => 3,
        };
        level <= maximum
    }

    const fn hwcap_priority(self, name: &str) -> Option<u8> {
        match (self, name.as_bytes()) {
            (Self::X86_64V4, b"x86-64-v4") => Some(0),
            (Self::X86_64V4 | Self::X86_64V3, b"x86-64-v3") => Some(1),
            (Self::X86_64V4 | Self::X86_64V3 | Self::X86_64V2, b"x86-64-v2") => Some(2),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxDynamicLoaderCacheSelection {
    Baseline,
    X86_64V2,
    X86_64V3,
    X86_64V4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LinuxDynamicLoaderCacheResolutionSummary {
    architecture: PersonalWorkerRuntimeArchitecture,
    selection: LinuxDynamicLoaderCacheSelection,
}

impl LinuxDynamicLoaderCacheResolutionSummary {
    #[must_use]
    pub const fn architecture(self) -> PersonalWorkerRuntimeArchitecture {
        self.architecture
    }

    #[must_use]
    pub const fn selection(self) -> LinuxDynamicLoaderCacheSelection {
        self.selection
    }
}

/// Opaque semantic result of one glibc 2.39 cache lookup.
///
/// This result is not filesystem evidence. The retained path is crate-private so a later Linux
/// observer can open it beneath a held root and prove the complete symlink/file binding before it
/// contributes to runtime evidence.
pub struct LinuxDynamicLoaderCacheResolution {
    summary: LinuxDynamicLoaderCacheResolutionSummary,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the next descriptor-bound observer consumes the selected private identity"
        )
    )]
    pub(crate) library_name: String,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the next descriptor-bound observer consumes the selected private identity"
        )
    )]
    pub(crate) library_path: String,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the next descriptor-bound observer consumes the selected private identity"
        )
    )]
    pub(crate) isa_level: u16,
}

impl LinuxDynamicLoaderCacheResolution {
    #[must_use]
    pub const fn summary(&self) -> LinuxDynamicLoaderCacheResolutionSummary {
        self.summary
    }
}

impl fmt::Debug for LinuxDynamicLoaderCacheResolution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinuxDynamicLoaderCacheResolution")
            .field("summary", &self.summary)
            .field(
                "private_cache_identity",
                &"<private-loader-cache-resolution>",
            )
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxDynamicLoaderCacheResolutionErrorKind {
    IdentityMismatch,
    InvalidLibraryName,
    Missing,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct LinuxDynamicLoaderCacheResolutionError {
    pub kind: LinuxDynamicLoaderCacheResolutionErrorKind,
    pub code: &'static str,
    pub message: &'static str,
}

impl fmt::Debug for LinuxDynamicLoaderCacheResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinuxDynamicLoaderCacheResolutionError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for LinuxDynamicLoaderCacheResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for LinuxDynamicLoaderCacheResolutionError {}

/// Reproduce one glibc 2.39 cache selection from already-validated semantic input.
///
/// The supplied capability profile is caller-owned semantics, not observation evidence. The
/// returned cache path remains untrusted until a later descriptor-bound observer opens and
/// revalidates it beneath the exact protected runtime root.
///
/// # Errors
///
/// Returns a fixed path-free error for architecture mismatch, an invalid library basename,
/// or absence of a cache entry usable by the supplied capability profile.
pub fn resolve_linux_dynamic_loader_cache(
    cache: &LinuxDynamicLoaderCache,
    profile: LinuxDynamicLoaderCapabilityProfile,
    library_name: &str,
) -> Result<LinuxDynamicLoaderCacheResolution, LinuxDynamicLoaderCacheResolutionError> {
    if cache.architecture != profile.architecture() {
        return Err(resolution_identity_error());
    }
    validate_component(library_name, MAX_LIBRARY_NAME_BYTES)
        .map_err(|_| resolution_name_error())?;
    cache_library_cmp(library_name, library_name).map_err(|_| resolution_name_error())?;

    let mut best_named: Option<(&LinuxDynamicLoaderCacheEntry, u8)> = None;
    let mut baseline = None;
    for entry in &cache.entries {
        if cache_library_cmp(library_name, &entry.library_name)
            .map_err(|_| resolution_name_error())?
            != Ordering::Equal
        {
            continue;
        }
        let Some(name) = entry.hwcap_name.as_deref() else {
            baseline.get_or_insert(entry);
            continue;
        };
        let Some(priority) = profile.hwcap_priority(name) else {
            continue;
        };
        if !profile.supports_isa_level(entry.isa_level) {
            continue;
        }
        if best_named.is_none_or(|(_, best_priority)| priority < best_priority) {
            best_named = Some((entry, priority));
        }
    }
    let selected = best_named
        .map(|(entry, _)| entry)
        .or(baseline)
        .ok_or_else(resolution_missing_error)?;
    let selection = match selected.hwcap_name.as_deref() {
        None => LinuxDynamicLoaderCacheSelection::Baseline,
        Some("x86-64-v2") => LinuxDynamicLoaderCacheSelection::X86_64V2,
        Some("x86-64-v3") => LinuxDynamicLoaderCacheSelection::X86_64V3,
        Some("x86-64-v4") => LinuxDynamicLoaderCacheSelection::X86_64V4,
        Some(_) => return Err(resolution_missing_error()),
    };
    Ok(LinuxDynamicLoaderCacheResolution {
        summary: LinuxDynamicLoaderCacheResolutionSummary {
            architecture: cache.architecture,
            selection,
        },
        library_name: selected.library_name.clone(),
        library_path: selected.library_path.clone(),
        isa_level: selected.isa_level,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxDynamicLoaderCacheErrorKind {
    Size,
    VersionIncompatible,
    Format,
    Architecture,
    UnsafePath,
    UnsupportedCapability,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct LinuxDynamicLoaderCacheError {
    pub kind: LinuxDynamicLoaderCacheErrorKind,
    pub code: &'static str,
    pub message: &'static str,
}

impl fmt::Debug for LinuxDynamicLoaderCacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinuxDynamicLoaderCacheError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for LinuxDynamicLoaderCacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for LinuxDynamicLoaderCacheError {}

/// Parse one complete, already-bounded glibc 2.39 cache without resolving any library.
///
/// # Errors
///
/// Rejects old, mixed, wrong-endian, malformed, unknown-architecture, unbounded, unsorted,
/// path-unsafe, or unsupported capability/extension input. Known generic x86 compatibility
/// entries are fully validated but omitted from the compatible entry view.
pub fn parse_linux_dynamic_loader_cache(
    bytes: &[u8],
    architecture: PersonalWorkerRuntimeArchitecture,
) -> Result<LinuxDynamicLoaderCache, LinuxDynamicLoaderCacheError> {
    if bytes.len() < HEADER_BYTES || bytes.len() > LINUX_DYNAMIC_LOADER_CACHE_MAX_BYTES {
        return Err(size_error());
    }
    if bytes.get(..CACHE_MAGIC.len()) != Some(CACHE_MAGIC) {
        return Err(version_error());
    }
    if bytes.get(CACHE_MAGIC.len()..CACHE_MAGIC.len() + CACHE_VERSION.len()) != Some(CACHE_VERSION)
    {
        return Err(version_error());
    }

    let entry_count = usize::try_from(read_u32(bytes, 20)?).map_err(|_| size_error())?;
    let string_table_bytes = usize::try_from(read_u32(bytes, 24)?).map_err(|_| size_error())?;
    if entry_count > MAX_ENTRIES
        || string_table_bytes == 0
        || string_table_bytes > MAX_STRING_TABLE_BYTES
    {
        return Err(size_error());
    }
    if bytes[28] != LITTLE_ENDIAN_FLAG
        || bytes[29..32].iter().any(|byte| *byte != 0)
        || bytes[36..48].iter().any(|byte| *byte != 0)
    {
        return Err(format_error());
    }

    let entries_bytes = entry_count
        .checked_mul(ENTRY_BYTES)
        .ok_or_else(size_error)?;
    let entries_end = HEADER_BYTES
        .checked_add(entries_bytes)
        .ok_or_else(size_error)?;
    let string_end = entries_end
        .checked_add(string_table_bytes)
        .ok_or_else(size_error)?;
    if string_end > bytes.len() {
        return Err(size_error());
    }
    let expected_extension_offset = align_up(string_end, 4)?;
    let extension_offset = usize::try_from(read_u32(bytes, 32)?).map_err(|_| size_error())?;
    if extension_offset != expected_extension_offset
        || extension_offset > bytes.len()
        || bytes[string_end..extension_offset]
            .iter()
            .any(|byte| *byte != 0)
    {
        return Err(format_error());
    }

    let extensions = parse_extensions(bytes, extension_offset, entries_end, string_end)?;
    let mut parsed_entries = Vec::with_capacity(entry_count);
    let mut seen_entries = BTreeSet::new();
    let mut referenced_hwcaps = BTreeSet::new();
    for index in 0..entry_count {
        let offset = HEADER_BYTES + index * ENTRY_BYTES;
        let flags = read_i32(bytes, offset)?;
        let compatible = cache_id_is_compatible(flags, architecture)?;
        if read_u32(bytes, offset + 12)? != 0 {
            return Err(format_error());
        }
        let library_name = read_cache_string(
            bytes,
            usize::try_from(read_u32(bytes, offset + 4)?).map_err(|_| size_error())?,
            entries_end,
            string_end,
            MAX_LIBRARY_NAME_BYTES,
        )?;
        validate_component(library_name, MAX_LIBRARY_NAME_BYTES)?;
        cache_library_cmp(library_name, library_name)?;
        let library_path = read_cache_string(
            bytes,
            usize::try_from(read_u32(bytes, offset + 8)?).map_err(|_| size_error())?,
            entries_end,
            string_end,
            MAX_LIBRARY_PATH_BYTES,
        )?;
        validate_absolute_path(library_path)?;

        let raw_hwcap = read_u64(bytes, offset + 16)?;
        let (hwcap_name, isa_level) = if raw_hwcap == 0 {
            (None, 0)
        } else {
            let upper_without_isa = (raw_hwcap >> 32) & !HWCAP_ISA_MASK;
            if upper_without_isa != (HWCAP_EXTENSION_BIT >> 32) {
                return Err(unsupported_capability_error());
            }
            let hwcap_index =
                usize::try_from(raw_hwcap & u64::from(u32::MAX)).map_err(|_| size_error())?;
            let name = extensions
                .hwcap_names
                .get(hwcap_index)
                .ok_or_else(format_error)?
                .clone();
            referenced_hwcaps.insert(hwcap_index);
            let isa_level =
                u16::try_from((raw_hwcap >> 32) & HWCAP_ISA_MASK).map_err(|_| format_error())?;
            (Some(name), isa_level)
        };
        validate_capability(architecture, hwcap_name.as_deref(), isa_level)?;

        let duplicate_key = (
            flags,
            library_name.to_owned(),
            library_path.to_owned(),
            hwcap_name.clone(),
            isa_level,
        );
        if !seen_entries.insert(duplicate_key.clone()) {
            return Err(format_error());
        }
        parsed_entries.push(ParsedCacheEntry {
            cache_id: duplicate_key.0,
            compatible,
            library_name: duplicate_key.1,
            library_path: duplicate_key.2,
            hwcap_name: duplicate_key.3,
            isa_level,
        });
    }
    if referenced_hwcaps.len() != extensions.hwcap_names.len() {
        return Err(format_error());
    }
    validate_entry_order(&parsed_entries)?;

    let ignored_incompatible_entry_count = parsed_entries
        .iter()
        .filter(|entry| !entry.compatible)
        .count();
    let entries = parsed_entries
        .into_iter()
        .filter(|entry| entry.compatible)
        .map(|entry| LinuxDynamicLoaderCacheEntry {
            library_name: entry.library_name,
            library_path: entry.library_path,
            hwcap_name: entry.hwcap_name,
            isa_level: entry.isa_level,
        })
        .collect();

    Ok(LinuxDynamicLoaderCache {
        architecture,
        entries,
        ignored_incompatible_entry_count,
        glibc_hwcap_names: extensions.hwcap_names,
    })
}

#[derive(Clone, PartialEq, Eq)]
struct ParsedCacheEntry {
    cache_id: i32,
    compatible: bool,
    library_name: String,
    library_path: String,
    hwcap_name: Option<String>,
    isa_level: u16,
}

fn cache_id_is_compatible(
    cache_id: i32,
    architecture: PersonalWorkerRuntimeArchitecture,
) -> Result<bool, LinuxDynamicLoaderCacheError> {
    match (architecture, cache_id) {
        (PersonalWorkerRuntimeArchitecture::Aarch64, AARCH64_CACHE_ID)
        | (PersonalWorkerRuntimeArchitecture::X86_64, X86_64_CACHE_ID) => Ok(true),
        // Noble's x86_64 cache contains generic ELF/libc6 entries for its installed i386
        // compatibility libraries. The admitted 64-bit loader compares flags to 0x303 and
        // ignores these entries, so retain them only while validating the complete cache.
        (PersonalWorkerRuntimeArchitecture::X86_64, GENERIC_ELF_LIBC6_CACHE_ID) => Ok(false),
        _ => Err(architecture_error()),
    }
}

fn validate_capability(
    architecture: PersonalWorkerRuntimeArchitecture,
    hwcap_name: Option<&str>,
    isa_level: u16,
) -> Result<(), LinuxDynamicLoaderCacheError> {
    match architecture {
        PersonalWorkerRuntimeArchitecture::X86_64 => {
            if isa_level > 3
                || hwcap_name
                    .is_some_and(|name| !matches!(name, "x86-64-v2" | "x86-64-v3" | "x86-64-v4"))
            {
                return Err(unsupported_capability_error());
            }
        }
        PersonalWorkerRuntimeArchitecture::Aarch64 => {
            if hwcap_name.is_some() || isa_level != 0 {
                return Err(unsupported_capability_error());
            }
        }
    }
    Ok(())
}

struct ParsedExtensions {
    hwcap_names: Vec<String>,
}

fn parse_extensions(
    bytes: &[u8],
    extension_offset: usize,
    string_start: usize,
    string_end: usize,
) -> Result<ParsedExtensions, LinuxDynamicLoaderCacheError> {
    if read_u32(bytes, extension_offset)? != EXTENSION_MAGIC {
        return Err(format_error());
    }
    let section_count =
        usize::try_from(read_u32(bytes, extension_offset + 4)?).map_err(|_| size_error())?;
    if !matches!(section_count, 1 | 2) {
        return Err(unsupported_capability_error());
    }
    let directory_bytes = section_count
        .checked_mul(EXTENSION_SECTION_BYTES)
        .and_then(|value| value.checked_add(EXTENSION_HEADER_BYTES))
        .ok_or_else(size_error)?;
    let directory_end = extension_offset
        .checked_add(directory_bytes)
        .ok_or_else(size_error)?;
    if directory_end > bytes.len() {
        return Err(size_error());
    }

    let generator = read_extension_section(bytes, extension_offset, 0)?;
    if generator.tag != EXTENSION_TAG_GENERATOR || generator.flags != 0 {
        return Err(format_error());
    }
    let hwcap = if section_count == 2 {
        let section = read_extension_section(bytes, extension_offset, 1)?;
        if section.tag != EXTENSION_TAG_GLIBC_HWCAPS
            || section.flags != 0
            || section.offset != directory_end
            || section.size == 0
            || section.size % 4 != 0
        {
            return Err(format_error());
        }
        Some(section)
    } else {
        None
    };

    let hwcap_end = match hwcap {
        Some(section) => section
            .offset
            .checked_add(section.size)
            .ok_or_else(size_error)?,
        None => directory_end,
    };
    if generator.offset != hwcap_end
        || generator.size == 0
        || generator.size > MAX_GENERATOR_BYTES
        || generator
            .offset
            .checked_add(generator.size)
            .ok_or_else(size_error)?
            != bytes.len()
        || bytes[generator.offset..]
            .iter()
            .any(|byte| !matches!(byte, b' '..=b'~'))
    {
        return Err(format_error());
    }

    let mut hwcap_names: Vec<String> = Vec::new();
    if let Some(section) = hwcap {
        let count = section.size / 4;
        if count > MAX_HWCAP_NAMES {
            return Err(size_error());
        }
        for index in 0..count {
            let string_offset = usize::try_from(read_u32(bytes, section.offset + index * 4)?)
                .map_err(|_| size_error())?;
            let name = read_cache_string(
                bytes,
                string_offset,
                string_start,
                string_end,
                MAX_HWCAP_NAME_BYTES,
            )?;
            validate_component(name, MAX_HWCAP_NAME_BYTES)?;
            if hwcap_names
                .last()
                .is_some_and(|previous| previous.as_str() >= name)
            {
                return Err(format_error());
            }
            hwcap_names.push(name.to_owned());
        }
    }
    Ok(ParsedExtensions { hwcap_names })
}

#[derive(Clone, Copy)]
struct ExtensionSection {
    tag: u32,
    flags: u32,
    offset: usize,
    size: usize,
}

fn read_extension_section(
    bytes: &[u8],
    extension_offset: usize,
    index: usize,
) -> Result<ExtensionSection, LinuxDynamicLoaderCacheError> {
    let offset = extension_offset + EXTENSION_HEADER_BYTES + index * EXTENSION_SECTION_BYTES;
    Ok(ExtensionSection {
        tag: read_u32(bytes, offset)?,
        flags: read_u32(bytes, offset + 4)?,
        offset: usize::try_from(read_u32(bytes, offset + 8)?).map_err(|_| size_error())?,
        size: usize::try_from(read_u32(bytes, offset + 12)?).map_err(|_| size_error())?,
    })
}

fn validate_entry_order(entries: &[ParsedCacheEntry]) -> Result<(), LinuxDynamicLoaderCacheError> {
    for pair in entries.windows(2) {
        let previous = &pair[0];
        let current = &pair[1];
        match cache_library_cmp(&previous.library_name, &current.library_name)? {
            Ordering::Less => return Err(format_error()),
            Ordering::Greater => continue,
            Ordering::Equal => {
                if previous.library_name != current.library_name {
                    return Err(format_error());
                }
            }
        }
        match previous.cache_id.cmp(&current.cache_id) {
            Ordering::Less => return Err(format_error()),
            Ordering::Greater => continue,
            Ordering::Equal => {}
        }
        match (&previous.hwcap_name, &current.hwcap_name) {
            (None, Some(_)) => return Err(format_error()),
            (Some(previous), Some(current)) if previous > current => return Err(format_error()),
            _ => {}
        }
    }
    Ok(())
}

fn cache_library_cmp(left: &str, right: &str) -> Result<Ordering, LinuxDynamicLoaderCacheError> {
    let left = left.as_bytes();
    let right = right.as_bytes();
    let mut left_index = 0;
    let mut right_index = 0;
    while left_index < left.len() {
        let left_byte = left[left_index];
        let right_byte = right.get(right_index).copied().unwrap_or(0);
        if left_byte.is_ascii_digit() {
            if !right_byte.is_ascii_digit() {
                return Ok(Ordering::Greater);
            }
            let (left_value, next_left) = decimal_run(left, left_index)?;
            let (right_value, next_right) = decimal_run(right, right_index)?;
            if left_value != right_value {
                return Ok(left_value.cmp(&right_value));
            }
            left_index = next_left;
            right_index = next_right;
        } else if right_byte.is_ascii_digit() {
            return Ok(Ordering::Less);
        } else if left_byte != right_byte {
            return Ok(left_byte.cmp(&right_byte));
        } else {
            left_index += 1;
            right_index += 1;
        }
    }
    Ok(0_u8.cmp(&right.get(right_index).copied().unwrap_or(0)))
}

fn decimal_run(bytes: &[u8], start: usize) -> Result<(u32, usize), LinuxDynamicLoaderCacheError> {
    let mut value = 0_u32;
    let mut index = start;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u32::from(bytes[index] - b'0')))
            .filter(|value| *value <= i32::MAX as u32)
            .ok_or_else(format_error)?;
        index += 1;
    }
    Ok((value, index))
}

fn read_cache_string(
    bytes: &[u8],
    offset: usize,
    string_start: usize,
    string_end: usize,
    max_bytes: usize,
) -> Result<&str, LinuxDynamicLoaderCacheError> {
    if offset < string_start || offset >= string_end {
        return Err(format_error());
    }
    let available = bytes.get(offset..string_end).ok_or_else(size_error)?;
    let terminator = available
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(format_error)?;
    if terminator == 0 || terminator > max_bytes {
        return Err(format_error());
    }
    std::str::from_utf8(&available[..terminator]).map_err(|_| format_error())
}

fn validate_component(value: &str, max_bytes: usize) -> Result<(), LinuxDynamicLoaderCacheError> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > max_bytes
        || !bytes[0].is_ascii_alphanumeric()
        || !bytes[bytes.len() - 1].is_ascii_alphanumeric()
        || bytes.iter().any(|byte| {
            !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
        })
        || value == "."
        || value == ".."
    {
        return Err(unsafe_path_error());
    }
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_digit() {
            let (_, next) = decimal_run(bytes, index)?;
            index = next;
        } else {
            index += 1;
        }
    }
    Ok(())
}

fn validate_absolute_path(value: &str) -> Result<(), LinuxDynamicLoaderCacheError> {
    if value.len() > MAX_LIBRARY_PATH_BYTES || !value.starts_with('/') || value.ends_with('/') {
        return Err(unsafe_path_error());
    }
    for component in value[1..].split('/') {
        validate_component(component, MAX_LIBRARY_NAME_BYTES)?;
    }
    Ok(())
}

fn align_up(value: usize, alignment: usize) -> Result<usize, LinuxDynamicLoaderCacheError> {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
        .ok_or_else(size_error)
}

fn read_i32(bytes: &[u8], offset: usize) -> Result<i32, LinuxDynamicLoaderCacheError> {
    Ok(i32::from_le_bytes(read_array(bytes, offset)?))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, LinuxDynamicLoaderCacheError> {
    Ok(u32::from_le_bytes(read_array(bytes, offset)?))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, LinuxDynamicLoaderCacheError> {
    Ok(u64::from_le_bytes(read_array(bytes, offset)?))
}

fn read_array<const N: usize>(
    bytes: &[u8],
    offset: usize,
) -> Result<[u8; N], LinuxDynamicLoaderCacheError> {
    bytes
        .get(offset..offset.checked_add(N).ok_or_else(size_error)?)
        .ok_or_else(size_error)?
        .try_into()
        .map_err(|_| size_error())
}

const fn error(
    kind: LinuxDynamicLoaderCacheErrorKind,
    code: &'static str,
    message: &'static str,
) -> LinuxDynamicLoaderCacheError {
    LinuxDynamicLoaderCacheError {
        kind,
        code,
        message,
    }
}

const fn size_error() -> LinuxDynamicLoaderCacheError {
    error(
        LinuxDynamicLoaderCacheErrorKind::Size,
        "dynamic_loader_cache_size",
        "dynamic-loader cache exceeds its canonical bounds",
    )
}

const fn version_error() -> LinuxDynamicLoaderCacheError {
    error(
        LinuxDynamicLoaderCacheErrorKind::VersionIncompatible,
        "dynamic_loader_cache_version_incompatible",
        "dynamic-loader cache format requires explicit migration",
    )
}

const fn format_error() -> LinuxDynamicLoaderCacheError {
    error(
        LinuxDynamicLoaderCacheErrorKind::Format,
        "dynamic_loader_cache_format",
        "dynamic-loader cache is malformed or noncanonical",
    )
}

const fn architecture_error() -> LinuxDynamicLoaderCacheError {
    error(
        LinuxDynamicLoaderCacheErrorKind::Architecture,
        "dynamic_loader_cache_architecture",
        "dynamic-loader cache contains a library for another architecture",
    )
}

const fn unsafe_path_error() -> LinuxDynamicLoaderCacheError {
    error(
        LinuxDynamicLoaderCacheErrorKind::UnsafePath,
        "dynamic_loader_cache_unsafe_path",
        "dynamic-loader cache contains an unsafe library identity or path",
    )
}

const fn unsupported_capability_error() -> LinuxDynamicLoaderCacheError {
    error(
        LinuxDynamicLoaderCacheErrorKind::UnsupportedCapability,
        "dynamic_loader_cache_unsupported_capability",
        "dynamic-loader cache uses unsupported capability or extension semantics",
    )
}

const fn resolution_error(
    kind: LinuxDynamicLoaderCacheResolutionErrorKind,
    code: &'static str,
    message: &'static str,
) -> LinuxDynamicLoaderCacheResolutionError {
    LinuxDynamicLoaderCacheResolutionError {
        kind,
        code,
        message,
    }
}

const fn resolution_identity_error() -> LinuxDynamicLoaderCacheResolutionError {
    resolution_error(
        LinuxDynamicLoaderCacheResolutionErrorKind::IdentityMismatch,
        "dynamic_loader_cache_resolution_identity_mismatch",
        "dynamic-loader cache and capability profile identities do not match",
    )
}

const fn resolution_name_error() -> LinuxDynamicLoaderCacheResolutionError {
    resolution_error(
        LinuxDynamicLoaderCacheResolutionErrorKind::InvalidLibraryName,
        "dynamic_loader_cache_resolution_invalid_name",
        "dynamic-loader cache lookup requires one valid bounded library basename",
    )
}

const fn resolution_missing_error() -> LinuxDynamicLoaderCacheResolutionError {
    resolution_error(
        LinuxDynamicLoaderCacheResolutionErrorKind::Missing,
        "dynamic_loader_cache_resolution_missing",
        "dynamic-loader cache contains no usable entry for the requested library",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_current_cache_preserves_entries_and_hwcaps() {
        let bytes = fixture(
            PersonalWorkerRuntimeArchitecture::X86_64,
            &[
                FixtureEntry::plain("libz.so.1", "/lib/x86_64-linux-gnu/libz.so.1"),
                FixtureEntry::hwcap(
                    "libc.so.6",
                    "/lib/x86_64-linux-gnu/glibc-hwcaps/x86-64-v3/libc.so.6",
                    0,
                    3,
                ),
                FixtureEntry::plain("libc.so.6", "/lib/x86_64-linux-gnu/libc.so.6"),
            ],
            &["x86-64-v3"],
        );
        let parsed =
            parse_linux_dynamic_loader_cache(&bytes, PersonalWorkerRuntimeArchitecture::X86_64)
                .expect("current loader cache");
        assert_eq!(parsed.entries().len(), 3);
        assert_eq!(parsed.glibc_hwcap_names(), &["x86-64-v3"]);
        assert_eq!(parsed.entries()[1].hwcap_name(), Some("x86-64-v3"));
        assert_eq!(parsed.entries()[1].isa_level(), 3);
        assert_eq!(parsed.ignored_incompatible_entry_count(), 0);
        let debug = format!("{parsed:?} {:?}", parsed.entries()[0]);
        assert!(!debug.contains("/lib/"));
        assert!(!debug.contains("libz"));
    }

    #[test]
    fn cache_resolution_matches_glibc_hwcaps_and_isa_priority() {
        let bytes = fixture(
            PersonalWorkerRuntimeArchitecture::X86_64,
            &[
                FixtureEntry::hwcap(
                    "libc.so.6",
                    "/lib/x86_64-linux-gnu/glibc-hwcaps/x86-64-v2/libc.so.6",
                    0,
                    1,
                ),
                FixtureEntry::hwcap(
                    "libc.so.6",
                    "/lib/x86_64-linux-gnu/glibc-hwcaps/x86-64-v3/libc.so.6",
                    1,
                    2,
                ),
                FixtureEntry::hwcap(
                    "libc.so.6",
                    "/lib/x86_64-linux-gnu/glibc-hwcaps/x86-64-v4/libc.so.6",
                    2,
                    3,
                ),
                FixtureEntry::plain("libc.so.6", "/lib/x86_64-linux-gnu/libc.so.6"),
            ],
            &["x86-64-v2", "x86-64-v3", "x86-64-v4"],
        );
        let parsed =
            parse_linux_dynamic_loader_cache(&bytes, PersonalWorkerRuntimeArchitecture::X86_64)
                .expect("x86 cache");
        for (profile, expected) in [
            (
                LinuxDynamicLoaderCapabilityProfile::X86_64Baseline,
                LinuxDynamicLoaderCacheSelection::Baseline,
            ),
            (
                LinuxDynamicLoaderCapabilityProfile::X86_64V2,
                LinuxDynamicLoaderCacheSelection::X86_64V2,
            ),
            (
                LinuxDynamicLoaderCapabilityProfile::X86_64V3,
                LinuxDynamicLoaderCacheSelection::X86_64V3,
            ),
            (
                LinuxDynamicLoaderCapabilityProfile::X86_64V4,
                LinuxDynamicLoaderCacheSelection::X86_64V4,
            ),
        ] {
            let resolution = resolve_linux_dynamic_loader_cache(&parsed, profile, "libc.so.6")
                .expect("resolve supported profile");
            assert_eq!(resolution.summary().selection(), expected);
            assert_eq!(resolution.summary().architecture(), profile.architecture());
            assert!(!format!("{resolution:?}").contains("/lib"));
        }
    }

    #[test]
    fn cache_resolution_requires_both_active_name_and_isa_level() {
        let bytes = fixture(
            PersonalWorkerRuntimeArchitecture::X86_64,
            &[
                FixtureEntry::hwcap(
                    "libc.so.6",
                    "/lib/x86_64-linux-gnu/glibc-hwcaps/x86-64-v2/libc.so.6",
                    0,
                    1,
                ),
                FixtureEntry::hwcap(
                    "libc.so.6",
                    "/lib/x86_64-linux-gnu/glibc-hwcaps/x86-64-v3/libc.so.6",
                    1,
                    3,
                ),
                FixtureEntry::plain("libc.so.6", "/lib/x86_64-linux-gnu/libc.so.6"),
            ],
            &["x86-64-v2", "x86-64-v3"],
        );
        let parsed =
            parse_linux_dynamic_loader_cache(&bytes, PersonalWorkerRuntimeArchitecture::X86_64)
                .expect("x86 cache");
        let resolution = resolve_linux_dynamic_loader_cache(
            &parsed,
            LinuxDynamicLoaderCapabilityProfile::X86_64V3,
            "libc.so.6",
        )
        .expect("fall back from incompatible v3 object to v2");
        assert_eq!(
            resolution.summary().selection(),
            LinuxDynamicLoaderCacheSelection::X86_64V2
        );
        assert_eq!(resolution.isa_level, 1);
    }

    #[test]
    fn cache_resolution_identity_name_and_missing_fail_path_free() {
        let bytes = fixture(
            PersonalWorkerRuntimeArchitecture::Aarch64,
            &[FixtureEntry::plain(
                "libc.so.6",
                "/lib/aarch64-linux-gnu/libc.so.6",
            )],
            &[],
        );
        let parsed =
            parse_linux_dynamic_loader_cache(&bytes, PersonalWorkerRuntimeArchitecture::Aarch64)
                .expect("AArch64 cache");
        let resolution = resolve_linux_dynamic_loader_cache(
            &parsed,
            LinuxDynamicLoaderCapabilityProfile::Aarch64Baseline,
            "libc.so.6",
        )
        .expect("resolve AArch64 baseline");
        assert_eq!(resolution.library_name, "libc.so.6");
        assert_eq!(resolution.library_path, "/lib/aarch64-linux-gnu/libc.so.6");
        assert_eq!(
            resolution.summary().selection(),
            LinuxDynamicLoaderCacheSelection::Baseline
        );

        let numeric_alias_bytes = fixture(
            PersonalWorkerRuntimeArchitecture::Aarch64,
            &[FixtureEntry::plain(
                "lib1.so",
                "/lib/aarch64-linux-gnu/lib1.so",
            )],
            &[],
        );
        let numeric_alias_cache = parse_linux_dynamic_loader_cache(
            &numeric_alias_bytes,
            PersonalWorkerRuntimeArchitecture::Aarch64,
        )
        .expect("numeric cache name");
        let numeric_alias = resolve_linux_dynamic_loader_cache(
            &numeric_alias_cache,
            LinuxDynamicLoaderCapabilityProfile::Aarch64Baseline,
            "lib01.so",
        )
        .expect("glibc comparison-equivalent lookup name");
        assert_eq!(numeric_alias.library_name, "lib1.so");

        for (result, expected) in [
            (
                resolve_linux_dynamic_loader_cache(
                    &parsed,
                    LinuxDynamicLoaderCapabilityProfile::X86_64Baseline,
                    "libc.so.6",
                ),
                LinuxDynamicLoaderCacheResolutionErrorKind::IdentityMismatch,
            ),
            (
                resolve_linux_dynamic_loader_cache(
                    &parsed,
                    LinuxDynamicLoaderCapabilityProfile::Aarch64Baseline,
                    "../libc.so.6",
                ),
                LinuxDynamicLoaderCacheResolutionErrorKind::InvalidLibraryName,
            ),
            (
                resolve_linux_dynamic_loader_cache(
                    &parsed,
                    LinuxDynamicLoaderCapabilityProfile::Aarch64Baseline,
                    "libmissing.so.1",
                ),
                LinuxDynamicLoaderCacheResolutionErrorKind::Missing,
            ),
        ] {
            let error = result.expect_err("cache resolution refusal");
            assert_eq!(error.kind, expected);
            let debug = format!("{error:?}");
            assert!(!debug.contains('/'));
            assert!(!debug.contains("libc"));
        }
    }

    #[test]
    fn x86_64_validates_but_does_not_expose_generic_compatibility_entries() {
        let bytes = fixture(
            PersonalWorkerRuntimeArchitecture::X86_64,
            &[
                FixtureEntry::plain("libz.so.1", "/lib/x86_64-linux-gnu/libz.so.1"),
                FixtureEntry::plain("libc.so.6", "/lib/x86_64-linux-gnu/libc.so.6"),
                FixtureEntry::generic_x86_compat("libc.so.6", "/lib/i386-linux-gnu/libc.so.6"),
            ],
            &[],
        );
        let parsed =
            parse_linux_dynamic_loader_cache(&bytes, PersonalWorkerRuntimeArchitecture::X86_64)
                .expect("mixed Noble x86 cache");
        assert_eq!(parsed.entries().len(), 2);
        assert_eq!(parsed.ignored_incompatible_entry_count(), 1);
        assert!(
            parsed
                .entries()
                .iter()
                .all(|entry| !entry.library_path().contains("i386"))
        );

        let mut wrong_order = bytes.clone();
        wrong_order[HEADER_BYTES + ENTRY_BYTES..HEADER_BYTES + ENTRY_BYTES + 4]
            .copy_from_slice(&GENERIC_ELF_LIBC6_CACHE_ID.to_le_bytes());
        wrong_order[HEADER_BYTES + 2 * ENTRY_BYTES..HEADER_BYTES + 2 * ENTRY_BYTES + 4]
            .copy_from_slice(&X86_64_CACHE_ID.to_le_bytes());
        assert_eq!(
            parse_linux_dynamic_loader_cache(
                &wrong_order,
                PersonalWorkerRuntimeArchitecture::X86_64,
            )
            .expect_err("same-name cache IDs must be descending")
            .kind,
            LinuxDynamicLoaderCacheErrorKind::Format,
        );

        let mut unknown = bytes;
        unknown[HEADER_BYTES..HEADER_BYTES + 4].copy_from_slice(&0x0803_i32.to_le_bytes());
        assert_eq!(
            parse_linux_dynamic_loader_cache(&unknown, PersonalWorkerRuntimeArchitecture::X86_64)
                .expect_err("unreviewed cache ID")
                .kind,
            LinuxDynamicLoaderCacheErrorKind::Architecture,
        );
    }

    #[test]
    fn architecture_capabilities_and_numeric_name_aliases_fail_closed() {
        let unknown_x86_hwcap = fixture(
            PersonalWorkerRuntimeArchitecture::X86_64,
            &[FixtureEntry::hwcap(
                "libc.so.6",
                "/lib/x86_64-linux-gnu/glibc-hwcaps/future/libc.so.6",
                0,
                0,
            )],
            &["future"],
        );
        let excessive_x86_isa = fixture(
            PersonalWorkerRuntimeArchitecture::X86_64,
            &[FixtureEntry::hwcap(
                "libc.so.6",
                "/lib/x86_64-linux-gnu/glibc-hwcaps/x86-64-v4/libc.so.6",
                0,
                4,
            )],
            &["x86-64-v4"],
        );
        let aarch64_named_hwcap = fixture(
            PersonalWorkerRuntimeArchitecture::Aarch64,
            &[FixtureEntry::hwcap(
                "libc.so.6",
                "/lib/aarch64-linux-gnu/glibc-hwcaps/future/libc.so.6",
                0,
                0,
            )],
            &["future"],
        );
        for (bytes, architecture) in [
            (unknown_x86_hwcap, PersonalWorkerRuntimeArchitecture::X86_64),
            (excessive_x86_isa, PersonalWorkerRuntimeArchitecture::X86_64),
            (
                aarch64_named_hwcap,
                PersonalWorkerRuntimeArchitecture::Aarch64,
            ),
        ] {
            assert_eq!(
                parse_linux_dynamic_loader_cache(&bytes, architecture)
                    .expect_err("unselectable architecture capability")
                    .kind,
                LinuxDynamicLoaderCacheErrorKind::UnsupportedCapability,
            );
        }

        let numeric_alias = fixture(
            PersonalWorkerRuntimeArchitecture::X86_64,
            &[
                FixtureEntry::plain("lib1.so", "/lib/x86_64-linux-gnu/lib1.so"),
                FixtureEntry::plain("lib01.so", "/lib/x86_64-linux-gnu/lib01.so"),
            ],
            &[],
        );
        assert_eq!(
            parse_linux_dynamic_loader_cache(
                &numeric_alias,
                PersonalWorkerRuntimeArchitecture::X86_64,
            )
            .expect_err("numeric comparison alias")
            .kind,
            LinuxDynamicLoaderCacheErrorKind::Format,
        );

        let numeric_overflow = fixture(
            PersonalWorkerRuntimeArchitecture::X86_64,
            &[FixtureEntry::plain(
                "lib2147483648.so",
                "/lib/x86_64-linux-gnu/lib2147483648.so",
            )],
            &[],
        );
        assert_eq!(
            parse_linux_dynamic_loader_cache(
                &numeric_overflow,
                PersonalWorkerRuntimeArchitecture::X86_64,
            )
            .expect_err("numeric comparator overflow")
            .kind,
            LinuxDynamicLoaderCacheErrorKind::Format,
        );
    }

    #[test]
    fn version_architecture_and_reserved_drift_fail_closed() {
        let original = fixture(
            PersonalWorkerRuntimeArchitecture::Aarch64,
            &[FixtureEntry::plain(
                "libc.so.6",
                "/lib/aarch64-linux-gnu/libc.so.6",
            )],
            &[],
        );
        for (bytes, expected) in [
            (
                {
                    let mut bytes = original.clone();
                    bytes[17] = b'2';
                    bytes
                },
                LinuxDynamicLoaderCacheErrorKind::VersionIncompatible,
            ),
            (
                {
                    let mut bytes = original.clone();
                    bytes[28] = 3;
                    bytes
                },
                LinuxDynamicLoaderCacheErrorKind::Format,
            ),
            (
                {
                    let mut bytes = original.clone();
                    bytes[29] = 1;
                    bytes
                },
                LinuxDynamicLoaderCacheErrorKind::Format,
            ),
            (
                {
                    let mut bytes = original.clone();
                    bytes[48..52].copy_from_slice(&X86_64_CACHE_ID.to_le_bytes());
                    bytes
                },
                LinuxDynamicLoaderCacheErrorKind::Architecture,
            ),
        ] {
            assert_eq!(
                parse_linux_dynamic_loader_cache(
                    &bytes,
                    PersonalWorkerRuntimeArchitecture::Aarch64,
                )
                .expect_err("loader cache drift")
                .kind,
                expected
            );
        }
    }

    #[test]
    fn malformed_ranges_paths_order_and_capabilities_fail_closed() {
        let original = fixture(
            PersonalWorkerRuntimeArchitecture::X86_64,
            &[
                FixtureEntry::plain("libz.so.1", "/lib/x86_64-linux-gnu/libz.so.1"),
                FixtureEntry::plain("libc.so.6", "/lib/x86_64-linux-gnu/libc.so.6"),
            ],
            &[],
        );
        let entries_end = HEADER_BYTES + 2 * ENTRY_BYTES;
        for (bytes, expected) in [
            (
                {
                    let mut bytes = original.clone();
                    bytes[52..56].copy_from_slice(&1_u32.to_le_bytes());
                    bytes
                },
                LinuxDynamicLoaderCacheErrorKind::Format,
            ),
            (
                {
                    let mut bytes = original.clone();
                    let first_key = read_u32(&bytes, 52).expect("first key");
                    let second_key = read_u32(&bytes, 52 + ENTRY_BYTES).expect("second key");
                    bytes[52..56].copy_from_slice(&second_key.to_le_bytes());
                    bytes[52 + ENTRY_BYTES..56 + ENTRY_BYTES]
                        .copy_from_slice(&first_key.to_le_bytes());
                    bytes
                },
                LinuxDynamicLoaderCacheErrorKind::Format,
            ),
            (
                {
                    let mut bytes = original.clone();
                    let value = usize::try_from(read_u32(&bytes, 56).expect("path offset"))
                        .expect("path usize");
                    bytes[value] = b'.';
                    bytes
                },
                LinuxDynamicLoaderCacheErrorKind::UnsafePath,
            ),
            (
                {
                    let mut bytes = original.clone();
                    bytes[64..72].copy_from_slice(&1_u64.to_le_bytes());
                    bytes
                },
                LinuxDynamicLoaderCacheErrorKind::UnsupportedCapability,
            ),
            (
                {
                    let mut bytes = original.clone();
                    bytes[entries_end] = 0xff;
                    bytes
                },
                LinuxDynamicLoaderCacheErrorKind::Format,
            ),
        ] {
            assert_eq!(
                parse_linux_dynamic_loader_cache(
                    &bytes,
                    PersonalWorkerRuntimeArchitecture::X86_64,
                )
                .expect_err("malformed loader cache")
                .kind,
                expected
            );
        }
    }

    #[test]
    fn disposable_noble_loader_cache_matches_the_closed_model() {
        if std::env::var("GLAEDA_DISPOSABLE_PROBE").as_deref() != Ok("1") {
            return;
        }
        let architecture = match std::env::consts::ARCH {
            "aarch64" => PersonalWorkerRuntimeArchitecture::Aarch64,
            "x86_64" => PersonalWorkerRuntimeArchitecture::X86_64,
            other => panic!("unsupported package-probe architecture: {other}"),
        };
        let bytes = std::fs::read("/etc/ld.so.cache").expect("read Noble loader cache");
        let parsed = parse_linux_dynamic_loader_cache(&bytes, architecture)
            .expect("parse Noble loader cache");
        assert!(!parsed.entries().is_empty());
        let profile = match architecture {
            PersonalWorkerRuntimeArchitecture::Aarch64 => {
                LinuxDynamicLoaderCapabilityProfile::Aarch64Baseline
            }
            PersonalWorkerRuntimeArchitecture::X86_64 => {
                LinuxDynamicLoaderCapabilityProfile::X86_64Baseline
            }
        };
        let resolution = resolve_linux_dynamic_loader_cache(&parsed, profile, "libc.so.6")
            .expect("resolve Noble baseline libc cache entry");
        assert_eq!(
            resolution.summary().selection(),
            LinuxDynamicLoaderCacheSelection::Baseline
        );
        assert!(!format!("{resolution:?}").contains("/lib"));
    }

    #[derive(Clone, Copy)]
    struct FixtureEntry<'a> {
        name: &'a str,
        path: &'a str,
        cache_id: Option<i32>,
        hwcap_index: Option<u32>,
        isa_level: u16,
    }

    impl<'a> FixtureEntry<'a> {
        const fn plain(name: &'a str, path: &'a str) -> Self {
            Self {
                name,
                path,
                cache_id: None,
                hwcap_index: None,
                isa_level: 0,
            }
        }

        const fn generic_x86_compat(name: &'a str, path: &'a str) -> Self {
            Self {
                name,
                path,
                cache_id: Some(GENERIC_ELF_LIBC6_CACHE_ID),
                hwcap_index: None,
                isa_level: 0,
            }
        }

        const fn hwcap(name: &'a str, path: &'a str, index: u32, isa_level: u16) -> Self {
            Self {
                name,
                path,
                cache_id: None,
                hwcap_index: Some(index),
                isa_level,
            }
        }
    }

    fn fixture(
        architecture: PersonalWorkerRuntimeArchitecture,
        entries: &[FixtureEntry<'_>],
        hwcap_names: &[&str],
    ) -> Vec<u8> {
        let entries_end = HEADER_BYTES + entries.len() * ENTRY_BYTES;
        let mut strings = Vec::new();
        let mut entry_offsets = Vec::new();
        for entry in entries {
            let name = entries_end + strings.len();
            strings.extend_from_slice(entry.name.as_bytes());
            strings.push(0);
            let path = entries_end + strings.len();
            strings.extend_from_slice(entry.path.as_bytes());
            strings.push(0);
            entry_offsets.push((name, path));
        }
        let mut hwcap_offsets = Vec::new();
        for name in hwcap_names {
            hwcap_offsets.push(entries_end + strings.len());
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
        }
        let extension_offset = (entries_end + strings.len() + 3) & !3;
        let section_count = if hwcap_names.is_empty() { 1 } else { 2 };
        let directory_end =
            extension_offset + EXTENSION_HEADER_BYTES + section_count * EXTENSION_SECTION_BYTES;
        let hwcap_bytes = hwcap_names.len() * 4;
        let generator = b"ldconfig fixture release version 2.39";
        let generator_offset = directory_end + hwcap_bytes;
        let mut bytes = vec![0_u8; generator_offset + generator.len()];
        bytes[..17].copy_from_slice(CACHE_MAGIC);
        bytes[17..20].copy_from_slice(CACHE_VERSION);
        write_u32(
            &mut bytes,
            20,
            u32::try_from(entries.len()).expect("entry count"),
        );
        write_u32(
            &mut bytes,
            24,
            u32::try_from(strings.len()).expect("strings"),
        );
        bytes[28] = LITTLE_ENDIAN_FLAG;
        write_u32(
            &mut bytes,
            32,
            u32::try_from(extension_offset).expect("extension offset"),
        );
        let cache_id = match architecture {
            PersonalWorkerRuntimeArchitecture::Aarch64 => AARCH64_CACHE_ID,
            PersonalWorkerRuntimeArchitecture::X86_64 => X86_64_CACHE_ID,
        };
        for (index, entry) in entries.iter().enumerate() {
            let offset = HEADER_BYTES + index * ENTRY_BYTES;
            bytes[offset..offset + 4]
                .copy_from_slice(&entry.cache_id.unwrap_or(cache_id).to_le_bytes());
            write_u32(
                &mut bytes,
                offset + 4,
                u32::try_from(entry_offsets[index].0).expect("name offset"),
            );
            write_u32(
                &mut bytes,
                offset + 8,
                u32::try_from(entry_offsets[index].1).expect("path offset"),
            );
            let hwcap = entry.hwcap_index.map_or(0, |hwcap_index| {
                HWCAP_EXTENSION_BIT | (u64::from(entry.isa_level) << 32) | u64::from(hwcap_index)
            });
            bytes[offset + 16..offset + 24].copy_from_slice(&hwcap.to_le_bytes());
        }
        bytes[entries_end..entries_end + strings.len()].copy_from_slice(&strings);
        write_u32(&mut bytes, extension_offset, EXTENSION_MAGIC);
        write_u32(
            &mut bytes,
            extension_offset + 4,
            u32::try_from(section_count).expect("sections"),
        );
        write_section(
            &mut bytes,
            extension_offset + 8,
            EXTENSION_TAG_GENERATOR,
            generator_offset,
            generator.len(),
        );
        if !hwcap_names.is_empty() {
            write_section(
                &mut bytes,
                extension_offset + 24,
                EXTENSION_TAG_GLIBC_HWCAPS,
                directory_end,
                hwcap_bytes,
            );
            for (index, offset) in hwcap_offsets.into_iter().enumerate() {
                write_u32(
                    &mut bytes,
                    directory_end + index * 4,
                    u32::try_from(offset).expect("hwcap string offset"),
                );
            }
        }
        bytes[generator_offset..].copy_from_slice(generator);
        bytes
    }

    fn write_section(
        bytes: &mut [u8],
        offset: usize,
        tag: u32,
        section_offset: usize,
        size: usize,
    ) {
        write_u32(bytes, offset, tag);
        write_u32(
            bytes,
            offset + 8,
            u32::try_from(section_offset).expect("section offset"),
        );
        write_u32(
            bytes,
            offset + 12,
            u32::try_from(size).expect("section size"),
        );
    }

    fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
}
