//! Generate `docs/MODULE_INVENTORY.md` from the source tree and gate the unreferenced-export count.
//!
//! Glaeda builds a rigorous kernel, exports it, and defers composition. That is a deliberate order
//! of work, but it accrues a real debt: an exported module nothing calls is reviewed surface that
//! costs compile, lint, test, and audit time without advancing a product boundary. The repository
//! already knows this shape — `docs/history/RETIRED_FEATURE_ISLANDS.md` retires islands whose "only
//! live references were their public library exports, their own tests, and the implementation map".
//!
//! The map it names was hand-written. It drifted to describing 91 of 200 exports and then stopped
//! being read at all. Hand-maintained inventories of a 200-module crate do not survive; this test
//! replaces the inventory with a generated one and turns the debt into a number that cannot move
//! without a reviewer seeing it.
//!
//! Two properties are proven here and nothing else:
//!
//! 1. `docs/MODULE_INVENTORY.md` equals what the current tree generates, so its counts are never
//!    stale. Regenerate with
//!    `GLAEDA_WRITE_MODULE_INVENTORY=1 cargo test --locked --test module_inventory`.
//! 2. The number of exported modules with no consumer outside their own files stays at
//!    [`UNREFERENCED_EXPORT_CEILING`]. Raising that constant is an explicit, reviewable edit.
//!
//! This test proves neither that a referenced module is well composed nor that an unreferenced one
//! should be deleted. It reports reachability; the owning product decision stays with the roadmap.
//!
//! # Why the scan is textual and `cfg`-blind
//!
//! A `cargo`-based reachability analysis answers only for the host platform. 11 of the exported
//! modules are `#[cfg(target_os = "linux")]` and 5 are `#[cfg(unix)]`, so a compiler-driven count
//! would disagree between an Apple-silicon control plane and the `ubuntu-24.04` Verify runner, and
//! would report Linux-only modules as unreferenced on macOS. Every `cfg`-gated consumer is a false
//! positive waiting to discredit the check.
//!
//! Resolving module paths textually is `cfg`-blind by construction: a consumer behind any `cfg`
//! still names the module, so it still counts. The scan errs consistently toward *referenced* — a
//! module is only reported unreferenced when its name resolves as a path segment in no other file
//! at all. That keeps the gated number free of false positives, at the cost of understating the
//! debt, which is the right direction for a number people must trust.
//!
//! The crate uses no identifier-concatenating macro (`paste!`, `concat_idents!`) and `src/lib.rs`
//! contains no `pub use`, so no module can be consumed without its name appearing in the consumer's
//! source. `strip_cfg_test_blocks` and the tier classification keep inline `#[cfg(test)]` code out
//! of the composition tiers without letting it create a false unreferenced report.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

const INVENTORY_PATH: &str = "docs/MODULE_INVENTORY.md";
const WRITE_ENVIRONMENT_VARIABLE: &str = "GLAEDA_WRITE_MODULE_INVENTORY";

/// The highest number of exported modules that may have no consumer outside their own files.
///
/// This is a ratchet, not a threshold. A fixed "at most N% uncomposed" number picked today would be
/// an invented budget; this one is the measured state of the tree at the commit that introduced it,
/// and it only ever moves down. Exporting a new module with no caller fails the gate, and composing
/// or retiring one fails it too until the constant is lowered to match — which is what stops the
/// ceiling from quietly becoming headroom.
///
/// Raise it only when a reviewed product decision accepts a new uncomposed kernel, and say which
/// module and which owning milestone in the same change.
const UNREFERENCED_EXPORT_CEILING: usize = 34;

/// Path roots scanned for consumers. `benches/` and `examples/` hold no Rust today; they are listed
/// so the scan stays correct the day they do.
const CONSUMER_ROOTS: [&str; 4] = ["src", "tests", "benches", "examples"];

/// Path prefixes that identify a crate-root path segment.
const CRATE_ROOTS: [&str; 3] = ["crate", "glaeda", "smolrunner"];

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// How an exported module is reached, in decreasing order of composition.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Reach {
    /// Reachable from `src/main.rs` or `src/bin/*.rs` through non-test library code.
    Binary,
    /// Named by other non-test library code, but no binary path reaches it.
    Library,
    /// Named only by `tests/` or by `#[cfg(test)]` code in another module.
    Tests,
    /// Named by no file outside its own definition.
    None,
}

impl Reach {
    fn label(self) -> &'static str {
        match self {
            Reach::Binary => "binary",
            Reach::Library => "library",
            Reach::Tests => "tests",
            Reach::None => "none",
        }
    }
}

struct Module {
    name: String,
    /// The `cfg` predicate guarding the export, or `all` when it is unconditional.
    gate: String,
    own_files: BTreeSet<PathBuf>,
    consumers: BTreeSet<PathBuf>,
    reach: Reach,
    /// A test that reads this module's source as text rather than calling it. Such a test keeps the
    /// module honest but composes nothing, so it does not count as a consumer. Recording it stops
    /// the next reviewer's `grep` from contradicting this document.
    source_text_only_readers: BTreeSet<PathBuf>,
}

// ---------------------------------------------------------------------------
// Lexical reduction
// ---------------------------------------------------------------------------

/// Replace comments and literals with spaces so only code positions remain.
///
/// Byte length is preserved for nothing in particular; the scan is positional only within the
/// result. Doc comments go too: a module named in prose is documentation, not composition.
fn strip_comments_and_literals(code: &str) -> String {
    let bytes = code.as_bytes();
    let mut out = String::with_capacity(code.len());
    let mut index = 0;
    while index < bytes.len() {
        let rest = &code[index..];
        if rest.starts_with("//") {
            let end = rest.find('\n').map_or(bytes.len() - index, |offset| offset);
            out.push(' ');
            index += end;
        } else if rest.starts_with("/*") {
            let mut depth = 1usize;
            let mut cursor = index + 2;
            while cursor < bytes.len() && depth > 0 {
                if code[cursor..].starts_with("/*") {
                    depth += 1;
                    cursor += 2;
                } else if code[cursor..].starts_with("*/") {
                    depth -= 1;
                    cursor += 2;
                } else {
                    cursor += next_character_width(code, cursor);
                }
            }
            out.push(' ');
            index = cursor;
        } else if let Some(width) = raw_string_width(code, index) {
            out.push(' ');
            index += width;
        } else if bytes[index] == b'"' {
            let mut cursor = index + 1;
            while cursor < bytes.len() {
                if bytes[cursor] == b'\\' {
                    cursor += 1 + next_character_width(code, (cursor + 1).min(bytes.len()));
                } else if bytes[cursor] == b'"' {
                    cursor += 1;
                    break;
                } else {
                    cursor += next_character_width(code, cursor);
                }
            }
            out.push(' ');
            index = cursor;
        } else if let Some(width) = character_literal_width(code, index) {
            out.push(' ');
            index += width;
        } else {
            let width = next_character_width(code, index);
            out.push_str(&code[index..index + width]);
            index += width;
        }
    }
    out
}

fn next_character_width(code: &str, index: usize) -> usize {
    code[index..].chars().next().map_or(1, char::len_utf8)
}

/// Width of a raw string literal starting at `index`, if one starts there.
fn raw_string_width(code: &str, index: usize) -> Option<usize> {
    let bytes = code.as_bytes();
    if bytes[index] != b'r' {
        return None;
    }
    // A raw string may not follow an identifier character (`for` ends in no `r` that matters, but
    // `substr"` is not a literal either way).
    if index > 0 && is_identifier_byte(bytes[index - 1]) {
        return None;
    }
    let mut hashes = 0usize;
    let mut cursor = index + 1;
    while cursor < bytes.len() && bytes[cursor] == b'#' {
        hashes += 1;
        cursor += 1;
    }
    if cursor >= bytes.len() || bytes[cursor] != b'"' {
        return None;
    }
    let terminator = format!("\"{}", "#".repeat(hashes));
    let end = code[cursor + 1..]
        .find(&terminator)
        .map_or(bytes.len(), |offset| cursor + 1 + offset + terminator.len());
    Some(end - index)
}

/// Width of a character literal starting at `index`, if one starts there rather than a lifetime.
fn character_literal_width(code: &str, index: usize) -> Option<usize> {
    let bytes = code.as_bytes();
    if bytes[index] != b'\'' {
        return None;
    }
    let mut cursor = index + 1;
    if cursor >= bytes.len() {
        return None;
    }
    if bytes[cursor] == b'\\' {
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor] != b'\'' {
            cursor += next_character_width(code, cursor);
        }
        return (cursor < bytes.len()).then_some(cursor + 1 - index);
    }
    cursor += next_character_width(code, cursor);
    (cursor < bytes.len() && bytes[cursor] == b'\'').then_some(cursor + 1 - index)
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Remove `#[cfg(test)]` items and their bodies from already-reduced code.
///
/// Inline unit tests are the repository's chosen proof style, so a module's own tests must not make
/// it look composed, and another module's tests must not promote it out of the `tests` tier.
fn strip_cfg_test_blocks(code: &str) -> String {
    const MARKER: &str = "#[cfg(test)]";
    let mut out = String::with_capacity(code.len());
    let mut rest = code;
    while let Some(start) = rest.find(MARKER) {
        out.push_str(&rest[..start]);
        let after = &rest[start + MARKER.len()..];
        let brace = after.find('{');
        let semicolon = after.find(';');
        match (brace, semicolon) {
            // A braced item — `mod tests { .. }`, `use crate::a::{B, C};`, a gated `fn` — ends at
            // the brace that closes it.
            (Some(open), semicolon) if semicolon.is_none_or(|end| open < end) => {
                let mut depth = 0usize;
                let mut cursor = open;
                let bytes = after.as_bytes();
                while cursor < bytes.len() {
                    match bytes[cursor] {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                cursor += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    cursor += 1;
                }
                rest = &after[cursor.min(after.len())..];
            }
            // A statement item — `use crate::a::B;`, `mod tests;` — ends at its semicolon.
            (_, Some(end)) => rest = &after[end + 1..],
            // A trailing attribute with neither: nothing further can be gated.
            (_, None) => rest = "",
        }
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------
// Path resolution
// ---------------------------------------------------------------------------

/// Expand one `use` tree into flat `::`-joined paths.
fn expand_use_tree(tree: &str, prefix: &str, out: &mut Vec<String>) {
    for part in split_top_level_commas(tree) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.find('{') {
            None => {
                let leaf = part.split(" as ").next().unwrap_or(part).trim();
                out.push(join_path(prefix, leaf));
            }
            Some(open) => {
                let head = part[..open].trim();
                let Some(close) = matching_brace(part, open) else {
                    continue;
                };
                expand_use_tree(&part[open + 1..close], &join_path(prefix, head), out);
            }
        }
    }
}

fn join_path(prefix: &str, segment: &str) -> String {
    let segment = segment.trim().trim_end_matches("::").trim();
    match (prefix.is_empty(), segment.is_empty()) {
        (true, _) => segment.to_owned(),
        (false, true) => prefix.to_owned(),
        (false, false) => format!("{prefix}::{segment}"),
    }
}

fn matching_brace(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    for (offset, byte) in bytes.iter().enumerate().skip(open) {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_top_level_commas(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (offset, byte) in text.as_bytes().iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                parts.push(&text[start..offset]);
                start = offset + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// Exported modules named as a path segment by this code.
///
/// Three forms resolve, and nothing else does:
///
/// - a `use` tree rooted at the crate, including brace groups and `as` renames;
/// - an explicit `crate::`/`glaeda::` path outside a `use`;
/// - a leading bare `module::` segment, which a crate-rooted `use` must already have brought in.
///
/// A segment preceded by `::` is never a crate-root module, which is what keeps `std::process::exit`
/// from counting as a reference to the exported `process` module.
fn referenced_modules(code: &str, modules: &BTreeSet<String>) -> BTreeSet<String> {
    let reduced = strip_comments_and_literals(code);
    let compact = compact_path_separators(&reduced);
    let mut found = BTreeSet::new();

    let mut rest = compact.as_str();
    while let Some(start) = find_keyword(rest, "use") {
        let after = &rest[start + "use".len()..];
        let Some(end) = after.find(';') else { break };
        let mut paths = Vec::new();
        expand_use_tree(&after[..end], "", &mut paths);
        for path in paths {
            let mut segments = path.split("::");
            let Some(root) = segments.next() else {
                continue;
            };
            if !CRATE_ROOTS.contains(&root.trim()) {
                continue;
            }
            if let Some(module) = segments.next()
                && modules.contains(module.trim())
            {
                found.insert(module.trim().to_owned());
            }
        }
        rest = &after[end..];
    }

    for (offset, _) in compact.match_indices("::") {
        let before = &compact[..offset];
        let segment_start = before
            .rfind(|byte: char| !(byte.is_ascii_alphanumeric() || byte == '_'))
            .map_or(0, |index| {
                index + before[index..].chars().next().map_or(1, char::len_utf8)
            });
        let segment = &before[segment_start..];
        if segment.is_empty() {
            continue;
        }
        // `a::b` — `b` is qualified by `a`, so it is not a crate-root module unless `a` is one.
        let qualified = segment_start >= 2 && before[..segment_start].ends_with("::");
        if qualified {
            let owner_end = segment_start - 2;
            let owner_start = before[..owner_end]
                .rfind(|byte: char| !(byte.is_ascii_alphanumeric() || byte == '_'))
                .map_or(0, |index| {
                    index + before[index..].chars().next().map_or(1, char::len_utf8)
                });
            if !CRATE_ROOTS.contains(&&before[owner_start..owner_end]) {
                continue;
            }
        }
        if modules.contains(segment) {
            found.insert(segment.to_owned());
        }
    }

    // `crate::module` with no trailing `::`, as in `use crate::module;` or a bare `crate::module`.
    for (offset, _) in compact.match_indices("::") {
        let before = &compact[..offset];
        let root_start = before
            .rfind(|byte: char| !(byte.is_ascii_alphanumeric() || byte == '_'))
            .map_or(0, |index| {
                index + before[index..].chars().next().map_or(1, char::len_utf8)
            });
        if !CRATE_ROOTS.contains(&&before[root_start..]) {
            continue;
        }
        let after = &compact[offset + 2..];
        let end = after
            .find(|byte: char| !(byte.is_ascii_alphanumeric() || byte == '_'))
            .unwrap_or(after.len());
        if modules.contains(&after[..end]) {
            found.insert(after[..end].to_owned());
        }
    }

    found
}

fn find_keyword(text: &str, keyword: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut from = 0usize;
    while let Some(offset) = text[from..].find(keyword) {
        let start = from + offset;
        let end = start + keyword.len();
        let before_ok = start == 0 || !is_identifier_byte(bytes[start - 1]);
        let after_ok = end >= bytes.len() || !is_identifier_byte(bytes[end]);
        if before_ok && after_ok {
            return Some(start);
        }
        from = end;
    }
    None
}

fn compact_path_separators(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    let mut rest = code;
    while let Some(offset) = rest.find("::") {
        let head = rest[..offset].trim_end_matches([' ', '\t', '\n', '\r']);
        out.push_str(head);
        out.push_str("::");
        rest = rest[offset + 2..].trim_start_matches([' ', '\t', '\n', '\r']);
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------
// Tree scan
// ---------------------------------------------------------------------------

/// Exported modules in declaration order, paired with the `cfg` predicate guarding each export.
fn exported_modules(lib_source: &str) -> Vec<(String, String)> {
    let mut exports = Vec::new();
    let mut pending_gate: Option<String> = None;
    for line in lib_source.lines() {
        let trimmed = line.trim();
        if let Some(predicate) = trimmed
            .strip_prefix("#[cfg(")
            .and_then(|rest| rest.strip_suffix(")]"))
        {
            pending_gate = Some(predicate.replace(' ', ""));
            continue;
        }
        if let Some(name) = trimmed
            .strip_prefix("pub mod ")
            .and_then(|rest| rest.strip_suffix(';'))
            && line.starts_with("pub mod ")
        {
            exports.push((
                name.to_owned(),
                pending_gate.take().unwrap_or_else(|| "all".to_owned()),
            ));
            continue;
        }
        if trimmed.starts_with("///") || trimmed.starts_with("#[") || trimmed.is_empty() {
            continue;
        }
        pending_gate = None;
    }
    exports
}

fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for relative in CONSUMER_ROOTS {
        collect_rust_sources(&root.join(relative), root, &mut files);
    }
    files.sort();
    files
}

fn collect_rust_sources(directory: &Path, root: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut entries: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_rust_sources(&path, root, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path.strip_prefix(root).unwrap_or(&path).to_path_buf());
        }
    }
}

fn is_test_source(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some("tests" | "benches" | "examples")
        )
    }) || path.file_name().is_some_and(|name| name == "tests.rs")
}

fn is_binary_source(path: &Path) -> bool {
    path == Path::new("src/main.rs")
        || (path.starts_with("src/bin") && path.extension().is_some_and(|value| value == "rs"))
}

fn own_files(root: &Path, module: &str) -> BTreeSet<PathBuf> {
    let mut files = BTreeSet::new();
    let flat = PathBuf::from("src").join(format!("{module}.rs"));
    if root.join(&flat).is_file() {
        files.insert(flat);
    }
    let directory = PathBuf::from("src").join(module);
    if root.join(&directory).is_dir() {
        let mut nested = Vec::new();
        collect_rust_sources(&root.join(&directory), root, &mut nested);
        files.extend(nested);
    }
    files
}

fn scan(root: &Path) -> Vec<Module> {
    let lib_source = fs::read_to_string(root.join("src/lib.rs"))
        .expect("src/lib.rs must be readable from the crate root");
    let exports = exported_modules(&lib_source);
    let names: BTreeSet<String> = exports.iter().map(|(name, _)| name.clone()).collect();
    assert_eq!(
        names.len(),
        exports.len(),
        "src/lib.rs must not export a module name twice"
    );

    let sources = rust_sources(root);
    let mut all_references: BTreeMap<PathBuf, BTreeSet<String>> = BTreeMap::new();
    let mut composition_references: BTreeMap<PathBuf, BTreeSet<String>> = BTreeMap::new();
    let mut source_text_reads: BTreeMap<PathBuf, String> = BTreeMap::new();
    for path in &sources {
        let code = fs::read_to_string(root.join(path))
            .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()));
        all_references.insert(path.clone(), referenced_modules(&code, &names));
        if !is_test_source(path) {
            let composition = strip_cfg_test_blocks(&strip_comments_and_literals(&code));
            composition_references.insert(path.clone(), referenced_modules(&composition, &names));
        }
        source_text_reads.insert(path.clone(), code);
    }

    let owners: BTreeMap<String, BTreeSet<PathBuf>> = names
        .iter()
        .map(|name| (name.clone(), own_files(root, name)))
        .collect();
    for (name, files) in &owners {
        assert!(
            !files.is_empty(),
            "exported module `{name}` resolves to no file under src/"
        );
    }
    let file_owner: BTreeMap<&PathBuf, &String> = owners
        .iter()
        .flat_map(|(name, files)| files.iter().map(move |file| (file, name)))
        .collect();

    let library = PathBuf::from("src/lib.rs");
    let mut consumers: BTreeMap<String, BTreeSet<PathBuf>> = names
        .iter()
        .map(|name| (name.clone(), BTreeSet::new()))
        .collect();
    for (path, references) in &all_references {
        if *path == library {
            continue;
        }
        for module in references {
            if owners[module].contains(path) {
                continue;
            }
            consumers
                .get_mut(module)
                .expect("reference resolves to an exported module")
                .insert(path.clone());
        }
    }

    // Reachability from the binaries through non-test library code.
    let mut edges: BTreeMap<&str, BTreeSet<&String>> = BTreeMap::new();
    let mut frontier: VecDeque<&String> = VecDeque::new();
    for (path, references) in &composition_references {
        if is_binary_source(path) {
            frontier.extend(references.iter());
        } else if let Some(owner) = file_owner.get(path) {
            let entry = edges.entry(owner.as_str()).or_default();
            entry.extend(references.iter().filter(|module| *module != *owner));
        }
    }
    let mut reachable: BTreeSet<&String> = BTreeSet::new();
    while let Some(module) = frontier.pop_front() {
        if !reachable.insert(module) {
            continue;
        }
        if let Some(next) = edges.get(module.as_str()) {
            frontier.extend(next.iter().copied());
        }
    }

    let mut library_referenced: BTreeSet<&String> = BTreeSet::new();
    for (path, references) in &composition_references {
        if *path == library || is_binary_source(path) {
            continue;
        }
        for module in references {
            if !owners[module].contains(path) {
                library_referenced.insert(names.get(module).expect("known module"));
            }
        }
    }

    exports
        .into_iter()
        .map(|(name, gate)| {
            let key = names.get(&name).expect("known module");
            let consumers = consumers.remove(&name).expect("known module");
            let reach = if reachable.contains(key) {
                Reach::Binary
            } else if library_referenced.contains(key) {
                Reach::Library
            } else if consumers.is_empty() {
                Reach::None
            } else {
                Reach::Tests
            };
            let needle = format!("src/{name}.rs");
            let source_text_only_readers = source_text_reads
                .iter()
                .filter(|(path, code)| {
                    !owners[&name].contains(*path)
                        && **path != library
                        && !consumers.contains(*path)
                        && code.contains(&needle)
                })
                .map(|(path, _)| path.clone())
                .collect();
            Module {
                name,
                gate,
                own_files: owners[key].clone(),
                consumers,
                reach,
                source_text_only_readers,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Document
// ---------------------------------------------------------------------------

fn render(modules: &[Module]) -> String {
    let count = |reach: Reach| {
        modules
            .iter()
            .filter(|module| module.reach == reach)
            .count()
    };
    let unreferenced: Vec<&Module> = modules
        .iter()
        .filter(|module| module.reach == Reach::None)
        .collect();

    let mut document = String::new();
    document.push_str(
        "# Module inventory\n\
         \n\
         <!-- Generated by tests/module_inventory.rs. Do not edit by hand. -->\n\
         \n\
         Every module `src/lib.rs` exports, and whether anything outside that module's own files\n\
         names it. Regenerate with:\n\
         \n\
         ```bash\n\
         GLAEDA_WRITE_MODULE_INVENTORY=1 cargo test --locked --test module_inventory\n\
         ```\n\
         \n\
         `tests/module_inventory.rs` fails when this document disagrees with the tree, so the counts\n\
         below are always those of the commit that contains them. It replaces the hand-maintained\n\
         capability map retired to\n\
         [`history/IMPLEMENTATION_MAP.md`](history/IMPLEMENTATION_MAP.md), which drifted to\n\
         describing 91 of 200 exports.\n\
         \n\
         This is an inventory. It grants no authority to refactor, delete, rename, expose, or call a\n\
         module, and it makes no claim that a module *should* be composed or retired. Product track\n\
         and classification live in [`ROADMAP.md`](ROADMAP.md) and the owning contract documents.\n\
         \n\
         ## Reach\n\
         \n\
         | reach | meaning |\n\
         | --- | --- |\n\
         | `binary` | reachable from `src/main.rs` or `src/bin/*.rs` through non-test library code |\n\
         | `library` | named by other non-test library code; no binary path reaches it |\n\
         | `tests` | named only by `tests/` or by `#[cfg(test)]` code in another module |\n\
         | `none` | named by no file outside its own definition |\n\
         \n\
         Resolution is textual and therefore `cfg`-blind: a Linux-gated consumer counts on every\n\
         platform, so these numbers are identical on an Apple-silicon control plane and the\n\
         `ubuntu-24.04` Verify runner. `src/lib.rs` is not a consumer; the export is the thing\n\
         being measured.\n\
         \n",
    );

    document.push_str("## Totals\n\n");
    document.push_str(&format!(
        "- Exported modules: **{}**\n- Reach: **{} binary**, **{} library**, **{} tests**, \
         **{} none**\n- Gate: at most {} exports may have reach `none` \
         (`UNREFERENCED_EXPORT_CEILING`)\n\n",
        modules.len(),
        count(Reach::Binary),
        count(Reach::Library),
        count(Reach::Tests),
        count(Reach::None),
        UNREFERENCED_EXPORT_CEILING,
    ));

    document.push_str(
        "## Exports with no consumer\n\
         \n\
         These are reviewed kernels that no binary, library module, test, bench, or example names.\n\
         The repository builds kernels first and defers composition on purpose, so this list is the\n\
         standing record of that deferral, not a defect list. Compose one, retire it through\n\
         [`history/RETIRED_FEATURE_ISLANDS.md`](history/RETIRED_FEATURE_ISLANDS.md), or leave it and\n\
         let the gate hold the line.\n\
         \n\
         | module | gate | files | note |\n\
         | --- | --- | --- | --- |\n",
    );
    for module in &unreferenced {
        let note = if module.source_text_only_readers.is_empty() {
            String::new()
        } else {
            let readers: Vec<String> = module
                .source_text_only_readers
                .iter()
                .map(|path| format!("`{}`", path.display()))
                .collect();
            format!("source read as text, not called, by {}", readers.join(", "))
        };
        document.push_str(&format!(
            "| `{}` | `{}` | {} | {} |\n",
            module.name,
            module.gate,
            module.own_files.len(),
            note,
        ));
    }

    document.push_str(
        "\n## Every export\n\
         \n\
         | module | gate | reach | consumer files |\n\
         | --- | --- | --- | --- |\n",
    );
    for module in modules {
        document.push_str(&format!(
            "| `{}` | `{}` | {} | {} |\n",
            module.name,
            module.gate,
            module.reach.label(),
            module.consumers.len(),
        ));
    }

    document
}

// ---------------------------------------------------------------------------
// Gates
// ---------------------------------------------------------------------------

#[test]
fn the_inventory_document_matches_the_source_tree() {
    let root = repository_root();
    let generated = render(&scan(&root));
    let path = root.join(INVENTORY_PATH);

    if std::env::var_os(WRITE_ENVIRONMENT_VARIABLE).is_some() {
        fs::write(&path, &generated).expect("the inventory must be writable");
        return;
    }

    let committed = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{INVENTORY_PATH} must exist: {error}"));
    assert_eq!(
        committed, generated,
        "{INVENTORY_PATH} is stale. It is generated, never hand-edited. Regenerate it with \
         `{WRITE_ENVIRONMENT_VARIABLE}=1 cargo test --locked --test module_inventory` and commit \
         the result."
    );
}

#[test]
fn unreferenced_exports_stay_at_the_recorded_ceiling() {
    let modules = scan(&repository_root());
    let unreferenced: Vec<&str> = modules
        .iter()
        .filter(|module| module.reach == Reach::None)
        .map(|module| module.name.as_str())
        .collect();
    let observed = unreferenced.len();

    assert!(
        observed <= UNREFERENCED_EXPORT_CEILING,
        "{observed} exported modules have no consumer, above the recorded ceiling of \
         {UNREFERENCED_EXPORT_CEILING}. A newly exported module with no caller is reviewed surface \
         that costs compile, lint, test, and audit time without reaching a product boundary. \
         Compose it, keep it crate-private until a caller exists, or raise \
         UNREFERENCED_EXPORT_CEILING in tests/module_inventory.rs and name the owning milestone in \
         the same change. Current list: {unreferenced:?}"
    );
    assert!(
        observed >= UNREFERENCED_EXPORT_CEILING,
        "{observed} exported modules have no consumer, below the recorded ceiling of \
         {UNREFERENCED_EXPORT_CEILING}. Lower UNREFERENCED_EXPORT_CEILING in \
         tests/module_inventory.rs to {observed} so the ceiling keeps tracking the tree instead of \
         becoming headroom."
    );
}

// ---------------------------------------------------------------------------
// Self-checks
// ---------------------------------------------------------------------------
//
// The gate above is only worth running if the scanner resolves what Rust resolves. These prove the
// two directions that would discredit it: a missed consumer would report composed work as debt, and
// a phantom consumer would hide real debt behind a number that never moves.

fn modules_named(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn a_consumer_is_seen_through_cfg_gates_use_trees_and_renames() {
    let modules = modules_named(&[
        "gated_module",
        "grouped_module",
        "nested_module",
        "renamed_module",
        "bare_module",
        "qualified_module",
        "plain_module",
    ]);
    let code = r#"
        #[cfg(target_os = "linux")]
        use crate::gated_module::Thing;
        use crate::{grouped_module::Value, nested_module::{deep::Leaf, Other}};
        use crate::renamed_module as shorthand;
        use crate::bare_module;

        fn call() {
            bare_module::enter();
            let _ = crate::qualified_module::Marker::new();
            glaeda::plain_module::run();
        }
    "#;
    let found = referenced_modules(code, &modules);
    assert_eq!(
        found,
        modules,
        "every crate-rooted path form must resolve to its module; missing {:?}",
        modules.difference(&found).collect::<Vec<_>>()
    );
}

#[test]
fn a_module_named_only_in_prose_literals_or_a_foreign_crate_is_not_a_consumer() {
    let modules = modules_named(&["process", "artifact", "plan"]);
    let code = r##"
        //! This module explains crate::artifact and its plan.
        /// See crate::plan for the planning contract.
        /* crate::process is documented elsewhere. */
        use std::process::Command;

        fn run() {
            let _ = std::process::exit(0);
            let _ = "crate::artifact::Sha256Digest";
            let _ = r#"crate::plan::Plan"#;
        }
    "##;
    assert_eq!(
        referenced_modules(code, &modules),
        BTreeSet::new(),
        "documentation, string literals, and `std::` paths must never count as composition"
    );
}

#[test]
fn inline_test_code_does_not_compose_a_module() {
    let modules = modules_named(&["kernel", "braced_kernel", "statement_kernel"]);
    let code = r#"
        // Both `use` shapes appear gated in this crate: the braced group ends at its closing
        // brace, the single import ends at its semicolon. Missing either would let inline test
        // code read as composition.
        #[cfg(test)]
        use crate::braced_kernel::{One, Two};
        #[cfg(test)]
        use crate::statement_kernel::Only;

        fn production() {}

        #[cfg(test)]
        mod tests {
            use crate::kernel::Thing;

            #[test]
            fn exercise() {
                let _ = crate::kernel::run();
            }
        }
    "#;
    assert_eq!(
        referenced_modules(code, &modules),
        modules,
        "the raw scan must still see inline test references, so they never create a false \
         `none` report"
    );
    assert_eq!(
        referenced_modules(
            &strip_cfg_test_blocks(&strip_comments_and_literals(code)),
            &modules
        ),
        BTreeSet::new(),
        "the composition scan must not let another module's inline tests promote a module out of \
         the `tests` tier"
    );
}

/// Run the whole scan over a tree whose answer is known by construction.
///
/// Without this the gate could pass by reporting a constant. The fixture contains one module of
/// each reach, including a transitively reached one and an island that is *mentioned* everywhere a
/// non-consumer can mention it.
#[test]
fn the_scan_separates_each_reach_on_a_tree_whose_answer_is_known() {
    let root = std::env::temp_dir().join(format!(
        "glaeda-module-inventory-selfcheck-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let write = |relative: &str, contents: &str| {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("fixture paths have a parent"))
            .expect("fixture directory must be creatable");
        fs::write(path, contents).expect("fixture file must be writable");
    };

    write(
        "src/lib.rs",
        "pub mod entry;\n\
         pub mod transitive;\n\
         pub mod only_tested;\n\
         pub mod island;\n",
    );
    write(
        "src/main.rs",
        "use glaeda::entry::start;\nfn main() { start(); }\n",
    );
    write(
        "src/entry.rs",
        "use crate::transitive::Deep;\npub fn start() -> Deep { Deep }\n",
    );
    write("src/transitive.rs", "pub struct Deep;\n");
    write("src/only_tested.rs", "pub struct Bench;\n");
    write(
        "src/island.rs",
        "//! Mentions crate::entry in prose only.\npub struct Alone;\n",
    );
    // Every way a file can name `island` without consuming it.
    write(
        "tests/contract.rs",
        "use glaeda::only_tested::Bench;\n\
         //! See glaeda::island for the kernel.\n\
         const NAME: &str = \"glaeda::island::Alone\";\n\
         #[test]\n\
         fn exercise() { let _ = Bench; let _ = NAME; }\n",
    );
    write(
        "src/uses_island_only_in_tests.rs",
        "#[cfg(test)]\nmod tests {\n    use crate::island::Alone;\n    #[test]\n    fn t() { let _ = Alone; }\n}\n",
    );

    let modules = scan(&root);
    let reach: BTreeMap<&str, Reach> = modules
        .iter()
        .map(|module| (module.name.as_str(), module.reach))
        .collect();
    let _ = fs::remove_dir_all(&root);

    assert_eq!(
        reach,
        BTreeMap::from([
            ("entry", Reach::Binary),
            ("transitive", Reach::Binary),
            ("only_tested", Reach::Tests),
            ("island", Reach::Tests),
        ]),
        "the scan must reach `transitive` through `entry`, keep `only_tested` out of the binary \
         tiers, and never promote `island` past the inline tests that are its only consumer"
    );
}

/// The gated number must be the tree's, not a constant the generator happens to agree with.
#[test]
fn the_recorded_ceiling_is_the_measured_tree() {
    let modules = scan(&repository_root());
    assert_eq!(
        modules
            .iter()
            .filter(|module| module.reach == Reach::None)
            .count(),
        UNREFERENCED_EXPORT_CEILING,
        "UNREFERENCED_EXPORT_CEILING must equal the measured tree; see \
         unreferenced_exports_stay_at_the_recorded_ceiling for how to move it"
    );

    // `doctor` is the CLI's own read surface, reached from `src/main.rs` and nothing else. If the
    // scan ever resolves a module to its own definition, this is where it shows.
    let doctor = modules
        .iter()
        .find(|module| module.name == "doctor")
        .expect("src/lib.rs exports `doctor`");
    assert_eq!(doctor.reach, Reach::Binary);
    assert!(
        doctor.consumers.contains(&PathBuf::from("src/main.rs")),
        "`doctor` must resolve to its binary consumer, found {:?}",
        doctor.consumers
    );
}
