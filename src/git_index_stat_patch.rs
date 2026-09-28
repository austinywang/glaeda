use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub const GIT_INDEX_STAT_PATCH_SCHEMA_VERSION: u8 = 1;
pub const MAX_GIT_INDEX_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_GIT_INDEX_ENTRIES: u32 = 200_000;
pub const MAX_GIT_INDEX_PATH_BYTES: usize = 4_096;

const HEADER_BYTES: usize = 12;
const ENTRY_FIXED_BYTES: usize = 62;
const CHECKSUM_BYTES: usize = 20;
const EXTENSION_HEADER_BYTES: usize = 8;
const INDEX_SIGNATURE: &[u8; 4] = b"DIRC";
const SUPPORTED_INDEX_VERSION: u32 = 2;
const EXTENDED_FLAG: u16 = 0x4000;
const PATH_LENGTH_MASK: u16 = 0x0fff;
const LONG_PATH_SENTINEL: usize = PATH_LENGTH_MASK as usize;
const TREE_EXTENSION: &[u8; 4] = b"TREE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitIndexStat {
    ctime_seconds: u32,
    ctime_nanoseconds: u32,
    mtime_seconds: u32,
    mtime_nanoseconds: u32,
    device: u32,
    inode: u32,
    mode: u32,
    uid: u32,
    gid: u32,
    size: u32,
}

impl GitIndexStat {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub const fn new(
        ctime_seconds: u32,
        ctime_nanoseconds: u32,
        mtime_seconds: u32,
        mtime_nanoseconds: u32,
        device: u32,
        inode: u32,
        mode: u32,
        uid: u32,
        gid: u32,
        size: u32,
    ) -> Self {
        Self {
            ctime_seconds,
            ctime_nanoseconds,
            mtime_seconds,
            mtime_nanoseconds,
            device,
            inode,
            mode,
            uid,
            gid,
            size,
        }
    }

    fn words(self) -> [u32; 10] {
        [
            self.ctime_seconds,
            self.ctime_nanoseconds,
            self.mtime_seconds,
            self.mtime_nanoseconds,
            self.device,
            self.inode,
            self.mode,
            self.uid,
            self.gid,
            self.size,
        ]
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct GitIndexStatUpdate {
    path: Vec<u8>,
    stat: GitIndexStat,
}

impl GitIndexStatUpdate {
    /// Construct one exact index-path stat update.
    ///
    /// Paths remain private binary Git pathnames. Public errors never echo rejected path bytes.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for an empty, NUL-containing, or oversized pathname.
    pub fn new(
        path: impl Into<Vec<u8>>,
        stat: GitIndexStat,
    ) -> Result<Self, GitIndexStatPatchError> {
        let path = path.into();
        validate_path(&path)?;
        Ok(Self { path, stat })
    }
}

impl fmt::Debug for GitIndexStatUpdate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitIndexStatUpdate")
            .field("path", &"<private-git-index-path>")
            .field("stat", &self.stat)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitIndexStatPatch {
    schema_version: u8,
    patched_entries: u32,
    extension_bytes: usize,
    bytes: Vec<u8>,
}

impl GitIndexStatPatch {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn patched_entries(&self) -> u32 {
        self.patched_entries
    }

    #[must_use]
    pub const fn extension_bytes(&self) -> usize {
        self.extension_bytes
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Patch only stat-cache words in one complete SHA-1 Git index v2 document.
///
/// The input checksum is verified before parsing. Every index entry must have exactly one update,
/// and no update may name an entry absent from the index. Object IDs, flags, stages, paths, padding,
/// and the reviewed optional `TREE` extension are preserved byte-for-byte. The trailing SHA-1
/// checksum is recomputed after the stat words change.
///
/// The first version deliberately refuses index v3/v4, extended entry flags, required/unknown
/// extensions, malformed padding, duplicate paths, incomplete update sets, and oversized input.
///
/// # Errors
///
/// Returns a fixed, path-free error when any input falls outside the reviewed index-v2 contract.
pub fn patch_git_index_v2_stats(
    index: &[u8],
    updates: &[GitIndexStatUpdate],
) -> Result<GitIndexStatPatch, GitIndexStatPatchError> {
    if index.len() < HEADER_BYTES + CHECKSUM_BYTES || index.len() > MAX_GIT_INDEX_BYTES {
        return Err(invalid_index_size());
    }
    if &index[..4] != INDEX_SIGNATURE {
        return Err(invalid_signature());
    }
    let version = read_u32(index, 4)?;
    if version != SUPPORTED_INDEX_VERSION {
        return Err(unsupported_version());
    }
    let entry_count = read_u32(index, 8)?;
    if entry_count > MAX_GIT_INDEX_ENTRIES {
        return Err(too_many_entries());
    }

    let checksum_offset = index.len() - CHECKSUM_BYTES;
    let expected_checksum = sha1(&index[..checksum_offset]);
    if index[checksum_offset..] != expected_checksum {
        return Err(checksum_mismatch());
    }

    let update_map = build_update_map(updates)?;
    if usize::try_from(entry_count).ok() != Some(update_map.len()) {
        return Err(update_set_mismatch());
    }

    let mut output = index.to_vec();
    let mut seen = BTreeSet::new();
    let mut offset = HEADER_BYTES;

    for _ in 0..entry_count {
        let entry_end_fixed = offset
            .checked_add(ENTRY_FIXED_BYTES)
            .ok_or_else(entry_truncated)?;
        if entry_end_fixed > checksum_offset {
            return Err(entry_truncated());
        }

        let flags = read_u16(index, offset + 60)?;
        if flags & EXTENDED_FLAG != 0 {
            return Err(extended_flags_unsupported());
        }

        let path_start = entry_end_fixed;
        let path_end = index[path_start..checksum_offset]
            .iter()
            .position(|byte| *byte == 0)
            .map(|relative| path_start + relative)
            .ok_or_else(path_terminator_missing)?;
        let path = &index[path_start..path_end];
        validate_path(path)?;
        validate_path_flag(flags, path.len())?;
        if !seen.insert(path.to_vec()) {
            return Err(duplicate_index_path());
        }

        let unpadded_len = ENTRY_FIXED_BYTES
            .checked_add(path.len())
            .and_then(|value| value.checked_add(1))
            .ok_or_else(entry_truncated)?;
        let padded_len = align_eight(unpadded_len).ok_or_else(entry_truncated)?;
        let entry_end = offset.checked_add(padded_len).ok_or_else(entry_truncated)?;
        if entry_end > checksum_offset || path_end >= entry_end {
            return Err(entry_truncated());
        }
        if index[path_end + 1..entry_end].iter().any(|byte| *byte != 0) {
            return Err(nonzero_entry_padding());
        }

        let stat = update_map.get(path).ok_or_else(update_set_mismatch)?;
        write_stat_words(&mut output[offset..offset + 40], **stat);
        offset = entry_end;
    }

    if seen.len() != update_map.len() {
        return Err(update_set_mismatch());
    }

    let extension_bytes = validate_extensions(index, offset, checksum_offset)?;
    let checksum = sha1(&output[..checksum_offset]);
    output[checksum_offset..].copy_from_slice(&checksum);

    Ok(GitIndexStatPatch {
        schema_version: GIT_INDEX_STAT_PATCH_SCHEMA_VERSION,
        patched_entries: entry_count,
        extension_bytes,
        bytes: output,
    })
}

fn build_update_map(
    updates: &[GitIndexStatUpdate],
) -> Result<BTreeMap<Vec<u8>, &GitIndexStat>, GitIndexStatPatchError> {
    if updates.len() > MAX_GIT_INDEX_ENTRIES as usize {
        return Err(too_many_entries());
    }
    let mut result = BTreeMap::new();
    for update in updates {
        validate_path(&update.path)?;
        if result.insert(update.path.clone(), &update.stat).is_some() {
            return Err(duplicate_update_path());
        }
    }
    Ok(result)
}

fn validate_path(path: &[u8]) -> Result<(), GitIndexStatPatchError> {
    if path.is_empty() || path.len() > MAX_GIT_INDEX_PATH_BYTES || path.contains(&0) {
        return Err(invalid_path());
    }
    Ok(())
}

fn validate_path_flag(flags: u16, path_len: usize) -> Result<(), GitIndexStatPatchError> {
    let encoded = usize::from(flags & PATH_LENGTH_MASK);
    let expected = path_len.min(LONG_PATH_SENTINEL);
    if encoded != expected {
        return Err(path_length_mismatch());
    }
    Ok(())
}

fn validate_extensions(
    index: &[u8],
    mut offset: usize,
    checksum_offset: usize,
) -> Result<usize, GitIndexStatPatchError> {
    let start = offset;
    let mut seen_tree = false;
    while offset < checksum_offset {
        let header_end = offset
            .checked_add(EXTENSION_HEADER_BYTES)
            .ok_or_else(extension_truncated)?;
        if header_end > checksum_offset {
            return Err(extension_truncated());
        }
        let signature: &[u8; 4] = index[offset..offset + 4]
            .try_into()
            .map_err(|_| extension_truncated())?;
        let size =
            usize::try_from(read_u32(index, offset + 4)?).map_err(|_| extension_truncated())?;
        let payload_end = header_end
            .checked_add(size)
            .ok_or_else(extension_truncated)?;
        if payload_end > checksum_offset {
            return Err(extension_truncated());
        }
        if signature != TREE_EXTENSION || seen_tree {
            return Err(unsupported_extension());
        }
        seen_tree = true;
        offset = payload_end;
    }
    if offset != checksum_offset {
        return Err(extension_truncated());
    }
    Ok(checksum_offset - start)
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, GitIndexStatPatchError> {
    let end = offset.checked_add(4).ok_or_else(entry_truncated)?;
    let raw: [u8; 4] = bytes
        .get(offset..end)
        .ok_or_else(entry_truncated)?
        .try_into()
        .map_err(|_| entry_truncated())?;
    Ok(u32::from_be_bytes(raw))
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, GitIndexStatPatchError> {
    let end = offset.checked_add(2).ok_or_else(entry_truncated)?;
    let raw: [u8; 2] = bytes
        .get(offset..end)
        .ok_or_else(entry_truncated)?
        .try_into()
        .map_err(|_| entry_truncated())?;
    Ok(u16::from_be_bytes(raw))
}

fn write_stat_words(target: &mut [u8], stat: GitIndexStat) {
    for (slot, value) in target.chunks_exact_mut(4).zip(stat.words()) {
        slot.copy_from_slice(&value.to_be_bytes());
    }
}

fn align_eight(value: usize) -> Option<usize> {
    value.checked_add(7).map(|value| value & !7)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitIndexStatPatchError {
    code: &'static str,
    message: &'static str,
}

impl GitIndexStatPatchError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for GitIndexStatPatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for GitIndexStatPatchError {}

const fn error(code: &'static str, message: &'static str) -> GitIndexStatPatchError {
    GitIndexStatPatchError { code, message }
}

const fn invalid_index_size() -> GitIndexStatPatchError {
    error(
        "git_index_size_invalid",
        "Git index size is outside the reviewed bound",
    )
}

const fn invalid_signature() -> GitIndexStatPatchError {
    error(
        "git_index_signature_invalid",
        "Git index signature is invalid",
    )
}

const fn unsupported_version() -> GitIndexStatPatchError {
    error(
        "git_index_version_unsupported",
        "Git index version is outside the reviewed v2 contract",
    )
}

const fn too_many_entries() -> GitIndexStatPatchError {
    error(
        "git_index_entry_count_invalid",
        "Git index entry count exceeds the reviewed bound",
    )
}

const fn checksum_mismatch() -> GitIndexStatPatchError {
    error(
        "git_index_checksum_mismatch",
        "Git index checksum does not match the document bytes",
    )
}

const fn update_set_mismatch() -> GitIndexStatPatchError {
    error(
        "git_index_update_set_mismatch",
        "Git index stat updates do not exactly cover the index entries",
    )
}

const fn duplicate_update_path() -> GitIndexStatPatchError {
    error(
        "git_index_update_path_duplicate",
        "Git index stat updates contain a duplicate path",
    )
}

const fn duplicate_index_path() -> GitIndexStatPatchError {
    error(
        "git_index_path_duplicate",
        "Git index contains a duplicate path outside the reviewed contract",
    )
}

const fn invalid_path() -> GitIndexStatPatchError {
    error(
        "git_index_path_invalid",
        "Git index path is outside the reviewed bound",
    )
}

const fn path_length_mismatch() -> GitIndexStatPatchError {
    error(
        "git_index_path_length_mismatch",
        "Git index path length flags do not match the entry pathname",
    )
}

const fn extended_flags_unsupported() -> GitIndexStatPatchError {
    error(
        "git_index_extended_flags_unsupported",
        "Git index extended entry flags are outside the reviewed contract",
    )
}

const fn entry_truncated() -> GitIndexStatPatchError {
    error(
        "git_index_entry_truncated",
        "Git index entry bytes are truncated or overflowed",
    )
}

const fn path_terminator_missing() -> GitIndexStatPatchError {
    error(
        "git_index_path_terminator_missing",
        "Git index entry path terminator is missing",
    )
}

const fn nonzero_entry_padding() -> GitIndexStatPatchError {
    error(
        "git_index_padding_invalid",
        "Git index entry padding is invalid",
    )
}

const fn extension_truncated() -> GitIndexStatPatchError {
    error(
        "git_index_extension_truncated",
        "Git index extension bytes are truncated or overflowed",
    )
}

const fn unsupported_extension() -> GitIndexStatPatchError {
    error(
        "git_index_extension_unsupported",
        "Git index contains an extension outside the reviewed contract",
    )
}

pub(crate) fn sha1(input: &[u8]) -> [u8; 20] {
    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut message = Vec::with_capacity(input.len().saturating_add(72));
    message.extend_from_slice(input);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    let mut h0 = 0x6745_2301_u32;
    let mut h1 = 0xefcd_ab89_u32;
    let mut h2 = 0x98ba_dcfe_u32;
    let mut h3 = 0x1032_5476_u32;
    let mut h4 = 0xc3d2_e1f0_u32;

    for chunk in message.chunks_exact(64) {
        let mut words = [0_u32; 80];
        for (index, slot) in words.iter_mut().take(16).enumerate() {
            let start = index * 4;
            *slot = u32::from_be_bytes([
                chunk[start],
                chunk[start + 1],
                chunk[start + 2],
                chunk[start + 3],
            ]);
        }
        for index in 16..80 {
            words[index] =
                (words[index - 3] ^ words[index - 8] ^ words[index - 14] ^ words[index - 16])
                    .rotate_left(1);
        }

        let mut a = h0;
        let mut b = h1;
        let mut c = h2;
        let mut d = h3;
        let mut e = h4;

        for (index, word) in words.into_iter().enumerate() {
            let (function, constant) = match index {
                0..=19 => ((b & c) | ((!b) & d), 0x5a82_7999),
                20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(function)
                .wrapping_add(e)
                .wrapping_add(constant)
                .wrapping_add(word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }

    let mut digest = [0_u8; 20];
    for (chunk, value) in digest.chunks_exact_mut(4).zip([h0, h1, h2, h3, h4]) {
        chunk.copy_from_slice(&value.to_be_bytes());
    }
    digest
}

#[cfg(test)]
mod tests {
    use super::{GitIndexStat, GitIndexStatUpdate, TREE_EXTENSION, patch_git_index_v2_stats, sha1};

    fn stat(seed: u32) -> GitIndexStat {
        GitIndexStat::new(
            seed,
            seed + 1,
            seed + 2,
            seed + 3,
            seed + 4,
            seed + 5,
            0o100644,
            seed + 6,
            seed + 7,
            seed + 8,
        )
    }

    fn fixture(entries: &[(&[u8], [u8; 20])], extension: Option<(&[u8; 4], &[u8])>) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"DIRC");
        bytes.extend_from_slice(&2_u32.to_be_bytes());
        bytes.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (path, oid) in entries {
            let start = bytes.len();
            bytes.extend_from_slice(&[0_u8; 40]);
            bytes.extend_from_slice(oid);
            let path_len = path.len().min(0x0fff);
            bytes.extend_from_slice(&(path_len as u16).to_be_bytes());
            bytes.extend_from_slice(path);
            bytes.push(0);
            while (bytes.len() - start) % 8 != 0 {
                bytes.push(0);
            }
        }
        if let Some((signature, payload)) = extension {
            bytes.extend_from_slice(signature);
            bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            bytes.extend_from_slice(payload);
        }
        let checksum = sha1(&bytes);
        bytes.extend_from_slice(&checksum);
        bytes
    }

    fn identities(index: &[u8]) -> Vec<(Vec<u8>, [u8; 20], u16)> {
        let count = u32::from_be_bytes(index[8..12].try_into().expect("header count"));
        let checksum_offset = index.len() - 20;
        let mut offset = 12;
        let mut result = Vec::new();
        for _ in 0..count {
            let oid: [u8; 20] = index[offset + 40..offset + 60]
                .try_into()
                .expect("entry oid");
            let flags = u16::from_be_bytes(
                index[offset + 60..offset + 62]
                    .try_into()
                    .expect("entry flags"),
            );
            let path_start = offset + 62;
            let path_end = index[path_start..checksum_offset]
                .iter()
                .position(|byte| *byte == 0)
                .map(|relative| path_start + relative)
                .expect("entry terminator");
            result.push((index[path_start..path_end].to_vec(), oid, flags));
            let raw_len = 62 + (path_end - path_start) + 1;
            offset += (raw_len + 7) & !7;
        }
        result
    }

    #[test]
    fn sha1_matches_standard_vectors() {
        assert_eq!(
            sha1(b""),
            [
                0xda, 0x39, 0xa3, 0xee, 0x5e, 0x6b, 0x4b, 0x0d, 0x32, 0x55, 0xbf, 0xef, 0x95, 0x60,
                0x18, 0x90, 0xaf, 0xd8, 0x07, 0x09,
            ]
        );
        assert_eq!(
            sha1(b"abc"),
            [
                0xa9, 0x99, 0x3e, 0x36, 0x47, 0x06, 0x81, 0x6a, 0xba, 0x3e, 0x25, 0x71, 0x78, 0x50,
                0xc2, 0x6c, 0x9c, 0xd0, 0xd8, 0x9d,
            ]
        );
    }

    #[test]
    fn patches_only_stat_words_and_checksum() {
        let entries = [
            (b"a.txt".as_slice(), [0x11; 20]),
            (b"dir/b.rs".as_slice(), [0x22; 20]),
        ];
        let input = fixture(&entries, Some((TREE_EXTENSION, b"opaque-cache-tree")));
        let before_identity = identities(&input);
        let before_extension =
            input[input.len() - 20 - (8 + b"opaque-cache-tree".len())..input.len() - 20].to_vec();
        let updates = [
            GitIndexStatUpdate::new(b"a.txt".to_vec(), stat(10)).expect("valid update"),
            GitIndexStatUpdate::new(b"dir/b.rs".to_vec(), stat(20)).expect("valid update"),
        ];

        let patch = patch_git_index_v2_stats(&input, &updates).expect("patch succeeds");
        assert_eq!(patch.schema_version(), 1);
        assert_eq!(patch.patched_entries(), 2);
        assert_eq!(patch.extension_bytes(), 8 + b"opaque-cache-tree".len());
        assert_eq!(identities(patch.bytes()), before_identity);
        let after_extension = &patch.bytes()
            [patch.bytes().len() - 20 - before_extension.len()..patch.bytes().len() - 20];
        assert_eq!(after_extension, before_extension);
        assert_ne!(&patch.bytes()[12..52], &input[12..52]);
        let checksum_offset = patch.bytes().len() - 20;
        assert_eq!(
            &patch.bytes()[checksum_offset..],
            sha1(&patch.bytes()[..checksum_offset])
        );
    }

    #[test]
    fn refuses_incomplete_duplicate_or_extra_updates() {
        let input = fixture(
            &[(b"a".as_slice(), [1; 20]), (b"b".as_slice(), [2; 20])],
            None,
        );
        let one = GitIndexStatUpdate::new(b"a".to_vec(), stat(1)).expect("valid update");
        assert_eq!(
            patch_git_index_v2_stats(&input, std::slice::from_ref(&one))
                .expect_err("missing update refused")
                .code(),
            "git_index_update_set_mismatch"
        );
        let duplicate = [one.clone(), one];
        assert_eq!(
            patch_git_index_v2_stats(&input, &duplicate)
                .expect_err("duplicate update refused")
                .code(),
            "git_index_update_path_duplicate"
        );
        let extra = [
            GitIndexStatUpdate::new(b"a".to_vec(), stat(1)).expect("valid update"),
            GitIndexStatUpdate::new(b"c".to_vec(), stat(2)).expect("valid update"),
        ];
        assert_eq!(
            patch_git_index_v2_stats(&input, &extra)
                .expect_err("wrong exact path set refused")
                .code(),
            "git_index_update_set_mismatch"
        );
    }

    #[test]
    fn refuses_bad_checksum_version_extended_flags_and_unreviewed_extension() {
        let base = fixture(&[(b"a".as_slice(), [1; 20])], None);
        let update = [GitIndexStatUpdate::new(b"a".to_vec(), stat(1)).expect("valid update")];

        let mut bad_checksum = base.clone();
        bad_checksum[15] ^= 1;
        assert_eq!(
            patch_git_index_v2_stats(&bad_checksum, &update)
                .expect_err("checksum mismatch refused")
                .code(),
            "git_index_checksum_mismatch"
        );

        let mut version_three = base.clone();
        version_three[4..8].copy_from_slice(&3_u32.to_be_bytes());
        let checksum_offset = version_three.len() - 20;
        let checksum = sha1(&version_three[..checksum_offset]);
        version_three[checksum_offset..].copy_from_slice(&checksum);
        assert_eq!(
            patch_git_index_v2_stats(&version_three, &update)
                .expect_err("v3 refused")
                .code(),
            "git_index_version_unsupported"
        );

        let mut extended = base.clone();
        let flags_offset = 12 + 60;
        let flags =
            u16::from_be_bytes(extended[flags_offset..flags_offset + 2].try_into().unwrap());
        extended[flags_offset..flags_offset + 2].copy_from_slice(&(flags | 0x4000).to_be_bytes());
        let checksum_offset = extended.len() - 20;
        let checksum = sha1(&extended[..checksum_offset]);
        extended[checksum_offset..].copy_from_slice(&checksum);
        assert_eq!(
            patch_git_index_v2_stats(&extended, &update)
                .expect_err("extended entry refused")
                .code(),
            "git_index_extended_flags_unsupported"
        );

        let unknown = fixture(&[(b"a".as_slice(), [1; 20])], Some((b"UNTR", b"opaque")));
        assert_eq!(
            patch_git_index_v2_stats(&unknown, &update)
                .expect_err("unreviewed extension refused")
                .code(),
            "git_index_extension_unsupported"
        );
    }

    #[test]
    fn refuses_malformed_padding_and_path_flags_without_echoing_paths() {
        let secret_like = b"private/project-secret".to_vec();
        let update = GitIndexStatUpdate::new(secret_like.clone(), stat(1)).expect("valid update");
        let mut input = fixture(&[(secret_like.as_slice(), [1; 20])], None);
        let path_end = 12 + 62 + secret_like.len();
        input[path_end + 1] = 1;
        let checksum_offset = input.len() - 20;
        let checksum = sha1(&input[..checksum_offset]);
        input[checksum_offset..].copy_from_slice(&checksum);
        let error = patch_git_index_v2_stats(&input, std::slice::from_ref(&update))
            .expect_err("nonzero padding refused");
        assert_eq!(error.code(), "git_index_padding_invalid");
        assert!(!error.to_string().contains("project-secret"));
        assert!(!format!("{error:?}").contains("project-secret"));

        let mut wrong_flags = fixture(&[(secret_like.as_slice(), [1; 20])], None);
        wrong_flags[12 + 60..12 + 62].copy_from_slice(&1_u16.to_be_bytes());
        let checksum_offset = wrong_flags.len() - 20;
        let checksum = sha1(&wrong_flags[..checksum_offset]);
        wrong_flags[checksum_offset..].copy_from_slice(&checksum);
        assert_eq!(
            patch_git_index_v2_stats(&wrong_flags, &[update])
                .expect_err("path flag mismatch refused")
                .code(),
            "git_index_path_length_mismatch"
        );
    }
}
