//! Pure, bounded parsing of the admitted Ubuntu Noble dynamic-loader configuration.
//!
//! This module does not open configuration files, expand an include, inspect a directory, parse
//! `ld.so.cache`, resolve a library, or construct runtime evidence. It only turns already-bounded
//! bytes into a closed semantic model for a later descriptor-bound R01 observer.

use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

use crate::personal_worker_runtime_contract::PersonalWorkerRuntimeArchitecture;

pub const LINUX_DYNAMIC_LOADER_CONFIG_MAX_BYTES: usize = 65_536;

const MAX_LINES: usize = 256;
const MAX_LINE_BYTES: usize = 4_096;
const ROOT_INCLUDE: &str = "include /etc/ld.so.conf.d/*.conf";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxDynamicLoaderConfigRole {
    Root,
    Fragment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LinuxDynamicLoaderSearchDirectory {
    Local,
    LocalMultiarch,
    LibMultiarch,
    UsrLibMultiarch,
}

impl LinuxDynamicLoaderSearchDirectory {
    fn parse(
        value: &str,
        architecture: PersonalWorkerRuntimeArchitecture,
    ) -> Result<Self, LinuxDynamicLoaderConfigError> {
        let directory = match (architecture, value) {
            (_, "/usr/local/lib") => Self::Local,
            (PersonalWorkerRuntimeArchitecture::Aarch64, "/usr/local/lib/aarch64-linux-gnu")
            | (PersonalWorkerRuntimeArchitecture::X86_64, "/usr/local/lib/x86_64-linux-gnu") => {
                Self::LocalMultiarch
            }
            (PersonalWorkerRuntimeArchitecture::Aarch64, "/lib/aarch64-linux-gnu")
            | (PersonalWorkerRuntimeArchitecture::X86_64, "/lib/x86_64-linux-gnu") => {
                Self::LibMultiarch
            }
            (PersonalWorkerRuntimeArchitecture::Aarch64, "/usr/lib/aarch64-linux-gnu")
            | (PersonalWorkerRuntimeArchitecture::X86_64, "/usr/lib/x86_64-linux-gnu") => {
                Self::UsrLibMultiarch
            }
            _ => return Err(unsafe_search_error()),
        };
        Ok(directory)
    }
}

/// Closed loader-configuration semantics parsed from one file.
///
/// This is not observation evidence: it carries no path identity, bytes digest, owner, mode,
/// include enumeration, directory metadata, loader-cache state, or revalidation proof.
#[derive(Clone, PartialEq, Eq)]
pub struct LinuxDynamicLoaderConfig {
    architecture: PersonalWorkerRuntimeArchitecture,
    role: LinuxDynamicLoaderConfigRole,
    includes_system_fragments: bool,
    search_directories: Vec<LinuxDynamicLoaderSearchDirectory>,
}

impl LinuxDynamicLoaderConfig {
    #[must_use]
    pub const fn architecture(&self) -> PersonalWorkerRuntimeArchitecture {
        self.architecture
    }

    #[must_use]
    pub const fn role(&self) -> LinuxDynamicLoaderConfigRole {
        self.role
    }

    #[must_use]
    pub const fn includes_system_fragments(&self) -> bool {
        self.includes_system_fragments
    }

    #[must_use]
    pub fn search_directories(&self) -> &[LinuxDynamicLoaderSearchDirectory] {
        &self.search_directories
    }
}

impl fmt::Debug for LinuxDynamicLoaderConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinuxDynamicLoaderConfig")
            .field("architecture", &self.architecture)
            .field("role", &self.role)
            .field("includes_system_fragments", &self.includes_system_fragments)
            .field("search_directory_count", &self.search_directories.len())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxDynamicLoaderConfigErrorKind {
    Size,
    Format,
    UnsafeSearch,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct LinuxDynamicLoaderConfigError {
    pub kind: LinuxDynamicLoaderConfigErrorKind,
    pub code: &'static str,
    pub message: &'static str,
}

impl fmt::Debug for LinuxDynamicLoaderConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinuxDynamicLoaderConfigError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for LinuxDynamicLoaderConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for LinuxDynamicLoaderConfigError {}

/// Parse one already-bounded Noble loader configuration file without expanding any include.
///
/// The root role accepts exactly one active `include /etc/ld.so.conf.d/*.conf` directive and no
/// search directory. The fragment role accepts only the four closed Noble directory identities
/// for the selected architecture and no include directive. Comments and empty lines are ignored;
/// all active syntax is canonical and order is retained.
///
/// # Errors
///
/// Rejects oversized, non-UTF-8, noncanonical, duplicate, wrong-role, wrong-architecture, or
/// otherwise unreviewed loader search input.
pub fn parse_linux_dynamic_loader_config(
    bytes: &[u8],
    architecture: PersonalWorkerRuntimeArchitecture,
    role: LinuxDynamicLoaderConfigRole,
) -> Result<LinuxDynamicLoaderConfig, LinuxDynamicLoaderConfigError> {
    if bytes.len() > LINUX_DYNAMIC_LOADER_CONFIG_MAX_BYTES {
        return Err(size_error());
    }
    if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
        return Err(format_error());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| format_error())?;
    if text.bytes().any(|byte| byte == 0 || byte == b'\r') {
        return Err(format_error());
    }

    let mut line_count = 0_usize;
    let mut includes_system_fragments = false;
    let mut seen_directories = BTreeSet::new();
    let mut search_directories = Vec::new();
    for line in text.split_terminator('\n') {
        line_count = line_count.checked_add(1).ok_or_else(size_error)?;
        if line_count > MAX_LINES || line.len() > MAX_LINE_BYTES {
            return Err(size_error());
        }
        if line.is_empty() || line.starts_with('#') {
            if line.bytes().any(|byte| !matches!(byte, b' '..=b'~')) {
                return Err(format_error());
            }
            continue;
        }
        if line == ROOT_INCLUDE {
            if role != LinuxDynamicLoaderConfigRole::Root || includes_system_fragments {
                return Err(format_error());
            }
            includes_system_fragments = true;
            continue;
        }
        let semantic_line = line.trim_start_matches(|character| {
            matches!(character, ' ' | '\t' | '\u{000b}' | '\u{000c}')
        });
        if semantic_line.starts_with("include") {
            return Err(unsafe_search_error());
        }
        if line.bytes().any(|byte| !matches!(byte, b'!'..=b'~')) {
            return Err(format_error());
        }
        if role != LinuxDynamicLoaderConfigRole::Fragment {
            return Err(format_error());
        }
        let directory = LinuxDynamicLoaderSearchDirectory::parse(line, architecture)?;
        if !seen_directories.insert(directory) {
            return Err(format_error());
        }
        search_directories.push(directory);
    }
    if role == LinuxDynamicLoaderConfigRole::Root && !includes_system_fragments {
        return Err(format_error());
    }
    Ok(LinuxDynamicLoaderConfig {
        architecture,
        role,
        includes_system_fragments,
        search_directories,
    })
}

const fn error(
    kind: LinuxDynamicLoaderConfigErrorKind,
    code: &'static str,
    message: &'static str,
) -> LinuxDynamicLoaderConfigError {
    LinuxDynamicLoaderConfigError {
        kind,
        code,
        message,
    }
}

const fn size_error() -> LinuxDynamicLoaderConfigError {
    error(
        LinuxDynamicLoaderConfigErrorKind::Size,
        "dynamic_loader_config_size",
        "dynamic-loader configuration exceeds its canonical bounds",
    )
}

const fn format_error() -> LinuxDynamicLoaderConfigError {
    error(
        LinuxDynamicLoaderConfigErrorKind::Format,
        "dynamic_loader_config_format",
        "dynamic-loader configuration is malformed or noncanonical",
    )
}

const fn unsafe_search_error() -> LinuxDynamicLoaderConfigError {
    error(
        LinuxDynamicLoaderConfigErrorKind::UnsafeSearch,
        "dynamic_loader_config_unsafe_search",
        "dynamic-loader configuration selects an unreviewed search path",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_root_and_architecture_fragments_preserve_closed_order() {
        let root = parse_linux_dynamic_loader_config(
            b"# Dynamic linker/loader configuration.\ninclude /etc/ld.so.conf.d/*.conf\n",
            PersonalWorkerRuntimeArchitecture::X86_64,
            LinuxDynamicLoaderConfigRole::Root,
        )
        .expect("root loader configuration");
        assert!(root.includes_system_fragments());
        assert!(root.search_directories().is_empty());

        for (architecture, bytes) in [
            (
                PersonalWorkerRuntimeArchitecture::Aarch64,
                b"# Multiarch support\n/usr/local/lib/aarch64-linux-gnu\n/lib/aarch64-linux-gnu\n/usr/lib/aarch64-linux-gnu\n"
                    .as_slice(),
            ),
            (
                PersonalWorkerRuntimeArchitecture::X86_64,
                b"# Multiarch support\n/usr/local/lib/x86_64-linux-gnu\n/lib/x86_64-linux-gnu\n/usr/lib/x86_64-linux-gnu\n"
                    .as_slice(),
            ),
        ] {
            let parsed = parse_linux_dynamic_loader_config(
                bytes,
                architecture,
                LinuxDynamicLoaderConfigRole::Fragment,
            )
            .expect("multiarch loader fragment");
            assert_eq!(
                parsed.search_directories(),
                &[
                    LinuxDynamicLoaderSearchDirectory::LocalMultiarch,
                    LinuxDynamicLoaderSearchDirectory::LibMultiarch,
                    LinuxDynamicLoaderSearchDirectory::UsrLibMultiarch,
                ]
            );
            let debug = format!("{parsed:?}");
            assert!(debug.contains("search_directory_count: 3"));
            assert!(!debug.contains("/usr/local"));
        }
    }

    #[test]
    fn comments_empty_fragments_and_exact_local_directory_are_supported() {
        let empty = parse_linux_dynamic_loader_config(
            b"# no additional directories\n\n",
            PersonalWorkerRuntimeArchitecture::Aarch64,
            LinuxDynamicLoaderConfigRole::Fragment,
        )
        .expect("empty fragment");
        assert!(empty.search_directories().is_empty());

        let local = parse_linux_dynamic_loader_config(
            b"# libc default configuration\n/usr/local/lib\n",
            PersonalWorkerRuntimeArchitecture::Aarch64,
            LinuxDynamicLoaderConfigRole::Fragment,
        )
        .expect("local directory fragment");
        assert_eq!(
            local.search_directories(),
            &[LinuxDynamicLoaderSearchDirectory::Local]
        );
    }

    #[test]
    fn include_role_duplicate_and_architecture_drift_fail_closed() {
        for (bytes, architecture, role, expected) in [
            (
                b"include /tmp/*.conf\n".as_slice(),
                PersonalWorkerRuntimeArchitecture::X86_64,
                LinuxDynamicLoaderConfigRole::Root,
                LinuxDynamicLoaderConfigErrorKind::UnsafeSearch,
            ),
            (
                b"  include /tmp/*.conf\n".as_slice(),
                PersonalWorkerRuntimeArchitecture::X86_64,
                LinuxDynamicLoaderConfigRole::Root,
                LinuxDynamicLoaderConfigErrorKind::UnsafeSearch,
            ),
            (
                b"\tinclude /etc/ld.so.conf.d/*.conf\n".as_slice(),
                PersonalWorkerRuntimeArchitecture::X86_64,
                LinuxDynamicLoaderConfigRole::Root,
                LinuxDynamicLoaderConfigErrorKind::UnsafeSearch,
            ),
            (
                b"\x0binclude /tmp/*.conf\n".as_slice(),
                PersonalWorkerRuntimeArchitecture::X86_64,
                LinuxDynamicLoaderConfigRole::Root,
                LinuxDynamicLoaderConfigErrorKind::UnsafeSearch,
            ),
            (
                b"\x0cinclude /tmp/*.conf\n".as_slice(),
                PersonalWorkerRuntimeArchitecture::X86_64,
                LinuxDynamicLoaderConfigRole::Root,
                LinuxDynamicLoaderConfigErrorKind::UnsafeSearch,
            ),
            (
                b"include /etc/ld.so.conf.d/*.conf\n".as_slice(),
                PersonalWorkerRuntimeArchitecture::X86_64,
                LinuxDynamicLoaderConfigRole::Fragment,
                LinuxDynamicLoaderConfigErrorKind::Format,
            ),
            (
                b"/usr/lib/x86_64-linux-gnu\n".as_slice(),
                PersonalWorkerRuntimeArchitecture::X86_64,
                LinuxDynamicLoaderConfigRole::Root,
                LinuxDynamicLoaderConfigErrorKind::Format,
            ),
            (
                b"/lib/aarch64-linux-gnu\n".as_slice(),
                PersonalWorkerRuntimeArchitecture::X86_64,
                LinuxDynamicLoaderConfigRole::Fragment,
                LinuxDynamicLoaderConfigErrorKind::UnsafeSearch,
            ),
            (
                b"/usr/local/lib\n/usr/local/lib\n".as_slice(),
                PersonalWorkerRuntimeArchitecture::X86_64,
                LinuxDynamicLoaderConfigRole::Fragment,
                LinuxDynamicLoaderConfigErrorKind::Format,
            ),
        ] {
            assert_eq!(
                parse_linux_dynamic_loader_config(bytes, architecture, role)
                    .expect_err("loader configuration drift")
                    .kind,
                expected
            );
        }
    }

    #[test]
    fn noncanonical_and_oversized_inputs_are_bounded() {
        for bytes in [
            b"/usr/local/lib".as_slice(),
            b" /usr/local/lib\n".as_slice(),
            b"/usr/local/lib \n".as_slice(),
            b"/usr/local/lib\r\n".as_slice(),
            b"/usr/local/lib\0\n".as_slice(),
            b"# tab\tcomment\n".as_slice(),
        ] {
            assert_eq!(
                parse_linux_dynamic_loader_config(
                    bytes,
                    PersonalWorkerRuntimeArchitecture::X86_64,
                    LinuxDynamicLoaderConfigRole::Fragment,
                )
                .expect_err("noncanonical loader configuration")
                .kind,
                LinuxDynamicLoaderConfigErrorKind::Format
            );
        }

        assert_eq!(
            parse_linux_dynamic_loader_config(
                &vec![b'#'; LINUX_DYNAMIC_LOADER_CONFIG_MAX_BYTES + 1],
                PersonalWorkerRuntimeArchitecture::X86_64,
                LinuxDynamicLoaderConfigRole::Fragment,
            )
            .expect_err("oversized loader configuration")
            .kind,
            LinuxDynamicLoaderConfigErrorKind::Size
        );
    }

    #[test]
    fn disposable_noble_loader_configs_match_the_closed_model() {
        if std::env::var("GLAEDA_DISPOSABLE_PROBE").as_deref() != Ok("1") {
            return;
        }
        let (architecture, multiarch_path) = match std::env::consts::ARCH {
            "aarch64" => (
                PersonalWorkerRuntimeArchitecture::Aarch64,
                "/etc/ld.so.conf.d/aarch64-linux-gnu.conf",
            ),
            "x86_64" => (
                PersonalWorkerRuntimeArchitecture::X86_64,
                "/etc/ld.so.conf.d/x86_64-linux-gnu.conf",
            ),
            other => panic!("unsupported package-probe architecture: {other}"),
        };
        for (path, role) in [
            ("/etc/ld.so.conf", LinuxDynamicLoaderConfigRole::Root),
            (
                "/etc/ld.so.conf.d/libc.conf",
                LinuxDynamicLoaderConfigRole::Fragment,
            ),
            (multiarch_path, LinuxDynamicLoaderConfigRole::Fragment),
        ] {
            let bytes = std::fs::read(path)
                .unwrap_or_else(|error| panic!("read loader configuration {path}: {error}"));
            parse_linux_dynamic_loader_config(&bytes, architecture, role)
                .unwrap_or_else(|error| panic!("parse loader configuration {path}: {error}"));
        }
    }
}
