//! Bounded parsing of the current Linux kernel advisory-lock table.
//!
//! A matching row proves only that the kernel snapshot contained one held exclusive whole-file
//! BSD lock. Absence from the snapshot is deliberately not exposed as proof that a file is
//! unlocked.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read as _;

use rustix::fs::{self as rustix_fs, Mode, OFlags};

const PROC_LOCKS: &str = "/proc/locks";
const MAX_KERNEL_LOCK_TABLE_BYTES: usize = 1_048_576;
const MAX_KERNEL_LOCK_TABLE_LINES: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct KernelFileIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

pub(crate) fn observe_exclusive_whole_file_flocks() -> Option<BTreeSet<KernelFileIdentity>> {
    let fd = rustix_fs::open(
        PROC_LOCKS,
        OFlags::RDONLY
            .union(OFlags::NOFOLLOW)
            .union(OFlags::CLOEXEC),
        Mode::empty(),
    )
    .ok()?;
    let mut bytes = Vec::new();
    File::from(fd)
        .take((MAX_KERNEL_LOCK_TABLE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    parse_exclusive_whole_file_flocks(&bytes)
}

fn parse_exclusive_whole_file_flocks(bytes: &[u8]) -> Option<BTreeSet<KernelFileIdentity>> {
    if bytes.len() > MAX_KERNEL_LOCK_TABLE_BYTES || (!bytes.is_empty() && !bytes.ends_with(b"\n")) {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let mut locks = BTreeSet::new();
    for (line_index, line) in text.lines().enumerate() {
        if line_index >= MAX_KERNEL_LOCK_TABLE_LINES || line.is_empty() {
            return None;
        }
        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        let sequence = fields.first()?.strip_suffix(':')?;
        sequence.parse::<u64>().ok()?;
        if fields.get(1) == Some(&"->") {
            continue;
        }
        if fields.get(1) != Some(&"FLOCK") {
            continue;
        }
        if fields.len() != 8 || fields[2] != "ADVISORY" || fields[4].parse::<u32>().is_err() {
            return None;
        }
        if fields[3] != "WRITE" || fields[6] != "0" || fields[7] != "EOF" {
            continue;
        }
        locks.insert(parse_file_identity(fields[5])?);
    }
    Some(locks)
}

fn parse_file_identity(value: &str) -> Option<KernelFileIdentity> {
    let mut fields = value.split(':');
    let major = u32::from_str_radix(fields.next()?, 16).ok()?;
    let minor = u32::from_str_radix(fields.next()?, 16).ok()?;
    let inode = fields.next()?.parse::<u64>().ok()?;
    if fields.next().is_some() {
        return None;
    }
    Some(KernelFileIdentity {
        device: rustix_fs::makedev(major, minor),
        inode,
    })
}

#[cfg(test)]
mod tests {
    use rustix::fs;

    use super::{KernelFileIdentity, parse_exclusive_whole_file_flocks};

    #[test]
    fn accepts_only_held_exclusive_whole_file_flocks() {
        let locks = parse_exclusive_whole_file_flocks(
            b"1: FLOCK  ADVISORY  WRITE 1788482 00:30:3745570 0 EOF\n\
              2: FLOCK  ADVISORY  READ  17 08:01:22 0 EOF\n\
              3: FLOCK  ADVISORY  WRITE 18 08:01:23 4 EOF\n\
              4: POSIX  ADVISORY  WRITE 19 08:01:24 0 EOF\n\
              5: -> FLOCK ADVISORY WRITE 20 08:01:25 0 EOF\n",
        )
        .expect("parse kernel lock fixture");

        assert_eq!(
            locks.into_iter().collect::<Vec<_>>(),
            vec![KernelFileIdentity {
                device: fs::makedev(0, 0x30),
                inode: 3_745_570,
            }]
        );
    }

    #[test]
    fn malformed_truncated_and_oversized_tables_are_unknown() {
        assert!(parse_exclusive_whole_file_flocks(b"not-a-row\n").is_none());
        assert!(
            parse_exclusive_whole_file_flocks(b"1: FLOCK ADVISORY WRITE 1 malformed 0 EOF\n")
                .is_none()
        );
        assert!(
            parse_exclusive_whole_file_flocks(b"1: FLOCK ADVISORY WRITE 1 00:30:1 0 EOF").is_none()
        );
        assert!(parse_exclusive_whole_file_flocks(&vec![b'x'; 1_048_577]).is_none());
    }
}
