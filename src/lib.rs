//! ShellInteropKit — a typed, lifetime-safe layer over the
//! Windows Shell, behind a small C ABI.
//!
//! What this solves: every file manager, launcher and
//! explorer-like app re-implements the same raw Shell
//! boilerplate — `SHGetFileInfoW` flag juggling, registry
//! walks for verbs, `AssocQueryStringW` two-pass buffers,
//! icon handles that must be destroyed, HRESULT /
//! `ShellExecute` return-code folklore. ShellInteropKit
//! does that once and hands back plain values:
//!
//! - **File info as one JSON object** (`si_file_info`):
//!   display name, extension, friendly type name,
//!   executable / directory flags, attribute names, size
//!   and modification time. The pure parts (extension,
//!   type table, attribute names, executable detection)
//!   work on every platform; on Windows the type name
//!   comes from the Shell itself when it has one.
//! - **Associations as JSON** (`si_association`, Windows):
//!   ProgID, friendly application name and open command
//!   for an extension, via `AssocQueryStringW`.
//! - **Verbs as a shaped JSON list** (`si_verbs`,
//!   Windows): the context-menu verbs registered for a
//!   file or folder, read from the registry, with the
//!   `&` accelerator stripped for display (and reported
//!   separately), duplicates removed, and the default
//!   verb marked.
//! - **Actions without structs** (`si_open`,
//!   `si_invoke_verb`, `si_show_properties`, Windows):
//!   all three go through `ShellExecuteW`, which needs
//!   no caller-built structure at all. There is
//!   deliberately no `ShellExecuteExW` wrapper: its
//!   `SHELLEXECUTEINFOW` layout is load-bearing, and a
//!   wrong field order is worse than one fewer function.
//! - **Icon presence without handle leaks**
//!   (`si_icon_info`, Windows): the Shell icon handle is
//!   obtained, its index read, and the handle destroyed
//!   inside the call. No handle ever crosses the ABI.
//!
//! Pure logic lives here and is tested everywhere;
//! Windows API access lives in `platform`; the C ABI
//! lives in `ffi`.

pub mod ffi;
pub mod platform;

pub const VERSION: &str = "0.1.0";
pub const VERSION_MAJOR: u32 = 0;
pub const VERSION_MINOR: u32 = 1;
pub const VERSION_PATCH: u32 = 0;

// Error codes (mirror include/shellinteropkit.h).
pub const SI_OK: i32 = 0;
pub const ERR_INVALID_ARG: i32 = 1;
pub const ERR_NOT_SUPPORTED: i32 = 2;
pub const ERR_NOT_FOUND: i32 = 3;
pub const ERR_ACCESS_DENIED: i32 = 4;
pub const ERR_PLATFORM: i32 = 5;
pub const ERR_INTERNAL: i32 = 6;

pub type SiResult<T> = Result<T, (i32, String)>;

/// Escape a string for inclusion inside JSON double quotes.
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

fn json_str(s: &str) -> String {
    format!("\"{}\"", json_escape(s))
}

fn json_opt_str(v: &Option<String>) -> String {
    match v {
        Some(s) => json_str(s),
        None => "null".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Attribute flags
// ---------------------------------------------------------------------------

// Win32 FILE_ATTRIBUTE_* values (winnt.h). Only the values
// this library is certain of are named; unknown bits are
// ignored rather than guessed at.
pub const FILE_ATTRIBUTE_READONLY: u32 = 0x0000_0001;
pub const FILE_ATTRIBUTE_HIDDEN: u32 = 0x0000_0002;
pub const FILE_ATTRIBUTE_SYSTEM: u32 = 0x0000_0004;
pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
pub const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x0000_0020;
pub const FILE_ATTRIBUTE_DEVICE: u32 = 0x0000_0040;
pub const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;
pub const FILE_ATTRIBUTE_TEMPORARY: u32 = 0x0000_0100;
pub const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x0000_0200;
pub const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
pub const FILE_ATTRIBUTE_COMPRESSED: u32 = 0x0000_0800;
pub const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
pub const FILE_ATTRIBUTE_NOT_CONTENT_INDEXED: u32 = 0x0000_2000;
pub const FILE_ATTRIBUTE_ENCRYPTED: u32 = 0x0000_4000;
pub const FILE_ATTRIBUTE_VIRTUAL: u32 = 0x0001_0000;

/// Map a Win32 attribute bitfield to names, in bit order.
/// `FILE_ATTRIBUTE_NORMAL` is reported only when it is the
/// sole bit set (that is its documented meaning: "no other
/// attributes set"). Unknown bits are ignored.
pub fn attribute_names(attrs: u32) -> Vec<&'static str> {
    const TABLE: &[(u32, &str)] = &[
        (FILE_ATTRIBUTE_READONLY, "readonly"),
        (FILE_ATTRIBUTE_HIDDEN, "hidden"),
        (FILE_ATTRIBUTE_SYSTEM, "system"),
        (FILE_ATTRIBUTE_DIRECTORY, "directory"),
        (FILE_ATTRIBUTE_ARCHIVE, "archive"),
        (FILE_ATTRIBUTE_DEVICE, "device"),
        (FILE_ATTRIBUTE_TEMPORARY, "temporary"),
        (FILE_ATTRIBUTE_SPARSE_FILE, "sparse_file"),
        (FILE_ATTRIBUTE_REPARSE_POINT, "reparse_point"),
        (FILE_ATTRIBUTE_COMPRESSED, "compressed"),
        (FILE_ATTRIBUTE_OFFLINE, "offline"),
        (FILE_ATTRIBUTE_NOT_CONTENT_INDEXED, "not_content_indexed"),
        (FILE_ATTRIBUTE_ENCRYPTED, "encrypted"),
        (FILE_ATTRIBUTE_VIRTUAL, "virtual"),
    ];
    if attrs == FILE_ATTRIBUTE_NORMAL {
        return vec!["normal"];
    }
    TABLE
        .iter()
        .filter(|(bit, _)| attrs & bit != 0)
        .map(|(_, name)| *name)
        .collect()
}

// ---------------------------------------------------------------------------
// Extensions, type names, executable detection
// ---------------------------------------------------------------------------

/// The extension of a path or file name: the text after
/// the last `.` in the final path component, lowercased,
/// without the dot. Empty when there is none — a leading
/// dot (`.gitignore`) is part of the name, not an
/// extension. Both `/` and `\` count as separators.
pub fn extension_of(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rfind('.') {
        Some(0) | None => String::new(),
        Some(i) => name[i + 1..].to_lowercase(),
    }
}

/// The display (file) name of a path: the final component,
/// or the whole path when it has no component (a root).
pub fn display_name_of(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return path.to_string();
    }
    match trimmed.rfind(['/', '\\']) {
        Some(i) => trimmed[i + 1..].to_string(),
        None => trimmed.to_string(),
    }
}

/// Extensions Windows treats as runnable. This is a
/// conservative, documented list — a file whose extension
/// is here is *capable* of being launched as a program or
/// script; it says nothing about whether it is safe.
pub const EXECUTABLE_EXTENSIONS: &[&str] = &[
    "exe",
    "com",
    "bat",
    "cmd",
    "msi",
    "msp",
    "ps1",
    "lnk",
    "scr",
    "pif",
    "application",
    "gadget",
    "msc",
    "cpl",
    "vbs",
    "vbe",
    "js",
    "jse",
    "wsf",
    "wsh",
];

/// True when `ext` (with or without a leading dot, any
/// case) names a runnable file type.
pub fn is_executable_extension(ext: &str) -> bool {
    let e = ext.trim_start_matches('.').to_lowercase();
    EXECUTABLE_EXTENSIONS.contains(&e.as_str())
}

/// The built-in fallback table of friendly type names.
/// This is a FALLBACK: on Windows `si_file_info` prefers
/// the Shell's own type name (`SHGFI_TYPENAME`) and uses
/// this table only when the Shell returns none. Keys are
/// lowercase extensions without the dot.
pub fn type_name_for_extension(ext: &str) -> Option<&'static str> {
    let e = ext.trim_start_matches('.').to_lowercase();
    let name = match e.as_str() {
        "txt" | "log" => "Text Document",
        "md" => "Markdown Document",
        "rtf" => "Rich Text Document",
        "doc" => "Microsoft Word Document",
        "docx" => "Microsoft Word Document",
        "xls" => "Microsoft Excel Worksheet",
        "xlsx" => "Microsoft Excel Worksheet",
        "ppt" => "Microsoft PowerPoint Presentation",
        "pptx" => "Microsoft PowerPoint Presentation",
        "pdf" => "PDF Document",
        "png" => "PNG Image",
        "jpg" | "jpeg" => "JPEG Image",
        "gif" => "GIF Image",
        "bmp" => "Bitmap Image",
        "svg" => "SVG Image",
        "ico" => "Icon File",
        "webp" => "WebP Image",
        "tif" | "tiff" => "TIFF Image",
        "mp3" => "MP3 Audio",
        "wav" => "WAV Audio",
        "flac" => "FLAC Audio",
        "ogg" => "Ogg Audio",
        "m4a" => "MPEG-4 Audio",
        "mp4" => "MPEG-4 Video",
        "mkv" => "Matroska Video",
        "avi" => "AVI Video",
        "mov" => "QuickTime Movie",
        "webm" => "WebM Video",
        "zip" => "ZIP Archive",
        "rar" => "RAR Archive",
        "7z" => "7-Zip Archive",
        "tar" => "TAR Archive",
        "gz" => "GZIP Archive",
        "exe" => "Application",
        "dll" => "Application Extension",
        "sys" => "System File",
        "bat" | "cmd" => "Windows Batch File",
        "ps1" => "PowerShell Script",
        "msi" => "Windows Installer Package",
        "lnk" => "Shortcut",
        "html" | "htm" => "HTML Document",
        "css" => "CSS File",
        "js" => "JavaScript File",
        "ts" => "TypeScript File",
        "json" => "JSON File",
        "xml" => "XML Document",
        "csv" => "CSV File",
        "rs" => "Rust Source File",
        "c" | "h" => "C Source File",
        "cpp" | "hpp" => "C++ Source File",
        "py" => "Python File",
        "java" => "Java Source File",
        "ini" => "Configuration File",
        "cfg" | "conf" => "Configuration File",
        "ttf" => "TrueType Font",
        "otf" => "OpenType Font",
        _ => return None,
    };
    Some(name)
}

/// Friendly type name for an extension, always returning
/// something displayable: the table name when known, the
/// Windows-style `"<EXT> File"` for an unknown extension,
/// or `"File"` when there is no extension.
pub fn fallback_type_name(ext: &str) -> String {
    let e = ext.trim_start_matches('.');
    match type_name_for_extension(e) {
        Some(n) => n.to_string(),
        None if e.is_empty() => "File".to_string(),
        None => format!("{} File", e.to_uppercase()),
    }
}

/// True when a path, file name or bare extension names a
/// runnable type. A leading-dot bare extension (`.bat`)
/// is read as the extension itself; anything else with a
/// dot is read by its final extension; a bare word is
/// read as an extension spelled without its dot.
pub fn is_executable_path_or_ext(s: &str) -> bool {
    if let Some(rest) = s.strip_prefix('.') {
        if !rest.is_empty() && !rest.contains(['.', '/', '\\']) {
            return is_executable_extension(rest);
        }
    }
    if s.contains('.') {
        is_executable_extension(&extension_of(s))
    } else {
        is_executable_extension(s)
    }
}

/// Normalize an association query argument to the dotted,
/// lowercase extension `AssocQueryStringW` expects
/// (`.txt`). Accepts `.txt`, `txt`, `file.txt` or a full
/// path; returns `None` when no extension can be found.
pub fn normalize_extension(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    // A bare word with no dot and no separator is an
    // extension spelled without its dot.
    if !trimmed.contains('.') && !trimmed.contains(['/', '\\']) {
        return Some(format!(".{}", trimmed.to_lowercase()));
    }
    // A leading-dot name with nothing after it but the
    // extension itself (".txt") IS the extension — unlike
    // a dotfile name in a path, there is nothing else here.
    if let Some(rest) = trimmed.strip_prefix('.') {
        if !rest.is_empty() && !rest.contains(['.', '/', '\\']) {
            return Some(format!(".{}", rest.to_lowercase()));
        }
    }
    let ext = extension_of(trimmed);
    if ext.is_empty() {
        None
    } else {
        Some(format!(".{ext}"))
    }
}

// ---------------------------------------------------------------------------
// File info
// ---------------------------------------------------------------------------

/// The `si_file_info` result before JSON shaping.
#[derive(Clone, Debug, PartialEq)]
pub struct FileInfo {
    pub exists: bool,
    pub display_name: String,
    pub extension: String,
    pub type_name: String,
    pub is_executable: bool,
    pub is_directory: bool,
    pub attributes: Vec<String>,
    pub size: u64,
    /// Seconds since the Unix epoch; `None` when the file
    /// does not exist or the platform gave no time.
    pub modified_unix: Option<u64>,
}

impl FileInfo {
    pub fn to_json(&self) -> String {
        let attrs: Vec<String> = self.attributes.iter().map(|a| json_str(a)).collect();
        let modified = match self.modified_unix {
            Some(v) => v.to_string(),
            None => "null".to_string(),
        };
        format!(
            "{{\"exists\":{},\"display_name\":{},\"extension\":{},\"type_name\":{},\"is_executable\":{},\"is_directory\":{},\"attributes\":[{}],\"size\":{},\"modified_unix\":{}}}",
            self.exists,
            json_str(&self.display_name),
            json_str(&self.extension),
            json_str(&self.type_name),
            self.is_executable,
            self.is_directory,
            attrs.join(","),
            self.size,
            modified
        )
    }
}

/// File info from `std::fs` metadata + the fallback type
/// table. This is the whole story off Windows; on Windows
/// the FFI layer replaces `type_name` with the Shell's
/// own name when the Shell provides one, and replaces
/// `attributes` with the real Win32 attribute names.
///
/// Off Windows the attribute list is derived, and the
/// derivation is deliberately small: `directory` when the
/// path is a folder and `readonly` when the metadata says
/// the file is not writable. Nothing else is claimed.
pub fn file_info_local(path: &str) -> FileInfo {
    let extension = extension_of(path);
    let display_name = display_name_of(path);
    let is_executable = is_executable_extension(&extension);
    match std::fs::metadata(path) {
        Ok(md) => {
            let is_directory = md.is_dir();
            #[cfg(windows)]
            let attributes: Vec<String> = {
                use std::os::windows::fs::MetadataExt;
                attribute_names(md.file_attributes())
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            };
            #[cfg(not(windows))]
            let attributes: Vec<String> = {
                let mut v: Vec<String> = Vec::new();
                if is_directory {
                    v.push("directory".to_string());
                }
                if md.permissions().readonly() {
                    v.push("readonly".to_string());
                }
                v
            };
            let modified_unix = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            FileInfo {
                exists: true,
                display_name,
                type_name: if is_directory {
                    "Folder".to_string()
                } else {
                    fallback_type_name(&extension)
                },
                extension,
                is_executable: is_executable && !is_directory,
                is_directory,
                attributes,
                size: if is_directory { 0 } else { md.len() },
                modified_unix,
            }
        }
        Err(_) => FileInfo {
            exists: false,
            display_name,
            type_name: fallback_type_name(&extension),
            extension,
            is_executable,
            is_directory: false,
            attributes: Vec::new(),
            size: 0,
            modified_unix: None,
        },
    }
}

// ---------------------------------------------------------------------------
// Verbs: label parsing, shaping, deduplication
// ---------------------------------------------------------------------------

/// One shell verb, shaped for display.
#[derive(Clone, Debug, PartialEq)]
pub struct Verb {
    pub name: String,
    /// Label with `&` accelerator markers removed (`&&`
    /// becomes a literal `&`).
    pub label: String,
    /// The accelerator character marked by `&` in the raw
    /// label, if any.
    pub accelerator: Option<char>,
    pub is_default: bool,
}

/// Split a raw registry verb label into its display text
/// and accelerator. Rules (the Windows menu convention):
/// `&x` marks `x` as the accelerator and is not displayed;
/// `&&` displays one literal `&` and marks nothing; a
/// trailing lone `&` is displayed as-is and marks nothing.
pub fn parse_verb_label(raw: &str) -> (String, Option<char>) {
    let mut display = String::with_capacity(raw.len());
    let mut accelerator: Option<char> = None;
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '&' {
            display.push(c);
            continue;
        }
        match chars.peek() {
            Some('&') => {
                chars.next();
                display.push('&');
            }
            Some(&next) => {
                chars.next();
                display.push(next);
                if accelerator.is_none() {
                    accelerator = Some(next);
                }
            }
            None => display.push('&'),
        }
    }
    (display, accelerator)
}

/// Shape raw `(name, raw_label)` registry pairs into the
/// verb list model: labels parsed, names deduplicated
/// case-insensitively (the first entry wins — registry
/// enumeration order is the priority order), empty names
/// dropped, and the verb whose name matches
/// `default_name` (case-insensitive) marked default. An
/// empty or missing label falls back to the verb name.
pub fn shape_verbs(raw: Vec<(String, String)>, default_name: Option<&str>) -> Vec<Verb> {
    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<Verb> = Vec::new();
    for (name, raw_label) in raw {
        let name = name.trim().to_string();
        if name.is_empty() {
            continue;
        }
        let key = name.to_lowercase();
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let label_source = if raw_label.trim().is_empty() {
            name.clone()
        } else {
            raw_label
        };
        let (label, accelerator) = parse_verb_label(&label_source);
        let is_default = default_name
            .map(|d| !d.is_empty() && d.eq_ignore_ascii_case(&name))
            .unwrap_or(false);
        out.push(Verb {
            name,
            label,
            accelerator,
            is_default,
        });
    }
    out
}

/// The `si_verbs` JSON shape: the verb list plus the name
/// of the default verb (or null when none is marked).
pub fn verbs_to_json(path: &str, verbs: &[Verb]) -> String {
    let default = verbs.iter().find(|v| v.is_default).map(|v| v.name.clone());
    let items: Vec<String> = verbs
        .iter()
        .map(|v| {
            let accel = match v.accelerator {
                Some(c) => json_str(&c.to_string()),
                None => "null".to_string(),
            };
            format!(
                "{{\"name\":{},\"label\":{},\"accelerator\":{},\"is_default\":{}}}",
                json_str(&v.name),
                json_str(&v.label),
                accel,
                v.is_default
            )
        })
        .collect();
    format!(
        "{{\"path\":{},\"default_verb\":{},\"verbs\":[{}]}}",
        json_str(path),
        json_opt_str(&default),
        items.join(",")
    )
}

// ---------------------------------------------------------------------------
// Association + icon result shapes
// ---------------------------------------------------------------------------

/// The `si_association` result before JSON shaping.
#[derive(Clone, Debug, PartialEq)]
pub struct Association {
    pub extension: String,
    pub prog_id: Option<String>,
    pub app_friendly_name: Option<String>,
    pub open_command: Option<String>,
}

impl Association {
    pub fn to_json(&self) -> String {
        format!(
            "{{\"extension\":{},\"prog_id\":{},\"app_friendly_name\":{},\"open_command\":{}}}",
            json_str(&self.extension),
            json_opt_str(&self.prog_id),
            json_opt_str(&self.app_friendly_name),
            json_opt_str(&self.open_command)
        )
    }
}

/// The `si_icon_info` JSON shape.
pub fn icon_info_json(icon_index: i32, has_icon: bool) -> String {
    format!("{{\"icon_index\":{icon_index},\"has_icon\":{has_icon}}}")
}

/// Map a `ShellExecuteW` result value (<= 32 means
/// failure) to this library's error code and a plain
/// message. The numbers are the documented `SE_ERR_*` /
/// Win32 codes ShellExecute returns in place of a handle.
pub fn shell_execute_error(value: usize) -> (i32, String) {
    let (code, text) = match value {
        0 => (
            ERR_PLATFORM,
            "the operating system is out of memory or resources",
        ),
        2 => (ERR_NOT_FOUND, "the specified file was not found"),
        3 => (ERR_NOT_FOUND, "the specified path was not found"),
        5 => (ERR_ACCESS_DENIED, "access to the specified file was denied"),
        8 => (
            ERR_PLATFORM,
            "there was not enough memory to complete the operation",
        ),
        11 => (ERR_PLATFORM, "the executable file is invalid"),
        26 => (ERR_PLATFORM, "a sharing violation occurred"),
        27 => (
            ERR_PLATFORM,
            "the file association is incomplete or invalid",
        ),
        28 => (ERR_PLATFORM, "the DDE transaction timed out"),
        29 => (ERR_PLATFORM, "the DDE transaction failed"),
        30 => (ERR_PLATFORM, "another DDE transaction is in progress"),
        31 => (
            ERR_NOT_FOUND,
            "there is no application associated with the file extension",
        ),
        32 => (ERR_PLATFORM, "the specified DLL was not found"),
        _ => (ERR_PLATFORM, "the shell could not perform the operation"),
    };
    (code, format!("ShellExecute failed ({value}): {text}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_json() {
        assert_eq!(json_escape("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
    }

    // -- Attributes --

    #[test]
    fn attributes_empty_and_normal() {
        assert!(attribute_names(0).is_empty());
        assert_eq!(attribute_names(FILE_ATTRIBUTE_NORMAL), vec!["normal"]);
        // NORMAL alongside another bit is NOT reported:
        // its meaning is "no other attributes set".
        assert_eq!(
            attribute_names(FILE_ATTRIBUTE_NORMAL | FILE_ATTRIBUTE_ARCHIVE),
            vec!["archive"]
        );
    }

    #[test]
    fn attributes_bit_order_and_names() {
        let v = attribute_names(
            FILE_ATTRIBUTE_READONLY
                | FILE_ATTRIBUTE_HIDDEN
                | FILE_ATTRIBUTE_SYSTEM
                | FILE_ATTRIBUTE_ARCHIVE,
        );
        assert_eq!(v, vec!["readonly", "hidden", "system", "archive"]);
        assert_eq!(attribute_names(FILE_ATTRIBUTE_DIRECTORY), vec!["directory"]);
        assert_eq!(
            attribute_names(FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_COMPRESSED),
            vec!["reparse_point", "compressed"]
        );
        assert_eq!(
            attribute_names(FILE_ATTRIBUTE_ENCRYPTED | FILE_ATTRIBUTE_TEMPORARY),
            vec!["temporary", "encrypted"]
        );
        // Unknown bits are ignored, never guessed at.
        assert!(attribute_names(0x2000_0000).is_empty());
    }

    // -- Extensions / type table / executables --

    #[test]
    fn extension_extraction() {
        assert_eq!(extension_of("notes.TXT"), "txt");
        assert_eq!(extension_of("/a/b/photo.Jpeg"), "jpeg");
        assert_eq!(extension_of("C:\\docs\\a.docx"), "docx");
        assert_eq!(extension_of("archive.tar.gz"), "gz");
        assert_eq!(extension_of("README"), "");
        assert_eq!(extension_of(".gitignore"), "");
        assert_eq!(extension_of("/a.b/c"), "");
        assert_eq!(extension_of("trailing."), "");
    }

    #[test]
    fn display_names() {
        assert_eq!(display_name_of("/a/b/c.txt"), "c.txt");
        assert_eq!(display_name_of("C:\\a\\b.txt"), "b.txt");
        assert_eq!(display_name_of("plain.txt"), "plain.txt");
        assert_eq!(display_name_of("/a/b/"), "b");
    }

    #[test]
    fn type_table_hits_and_fallback() {
        assert_eq!(type_name_for_extension(".txt"), Some("Text Document"));
        assert_eq!(type_name_for_extension("PNG"), Some("PNG Image"));
        assert_eq!(type_name_for_extension("pdf"), Some("PDF Document"));
        assert_eq!(type_name_for_extension("exe"), Some("Application"));
        assert_eq!(type_name_for_extension("rs"), Some("Rust Source File"));
        assert_eq!(type_name_for_extension("xyzqq"), None);
        assert_eq!(fallback_type_name("xyzqq"), "XYZQQ File");
        assert_eq!(fallback_type_name(""), "File");
        assert_eq!(fallback_type_name(".md"), "Markdown Document");
    }

    #[test]
    fn executable_matrix() {
        for e in [
            "exe", ".exe", "COM", "bat", "cmd", "msi", "ps1", "lnk", "scr", "pif", "vbs", "js",
            "msc", "cpl",
        ] {
            assert!(is_executable_extension(e), "{e} should be executable");
        }
        for e in ["txt", "png", "pdf", "zip", "dll", "rs", "", "docx"] {
            assert!(!is_executable_extension(e), "{e} should not be executable");
        }
    }

    #[test]
    fn normalize_extension_forms() {
        assert_eq!(normalize_extension(".txt").as_deref(), Some(".txt"));
        assert_eq!(normalize_extension("TXT").as_deref(), Some(".txt"));
        assert_eq!(normalize_extension("file.PDF").as_deref(), Some(".pdf"));
        assert_eq!(
            normalize_extension("C:\\a\\b.docx").as_deref(),
            Some(".docx")
        );
        assert_eq!(normalize_extension("noext").as_deref(), Some(".noext"));
        assert_eq!(normalize_extension("/a/b/README"), None);
        assert_eq!(normalize_extension(""), None);
        assert_eq!(normalize_extension("  "), None);
    }

    // -- File info on real files --

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("shellinteropkit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn file_info_real_file() {
        let dir = temp_dir("file");
        let file = dir.join("notes.txt");
        std::fs::write(&file, b"hello").unwrap();
        let info = file_info_local(file.to_str().unwrap());
        assert!(info.exists);
        assert_eq!(info.display_name, "notes.txt");
        assert_eq!(info.extension, "txt");
        assert_eq!(info.type_name, "Text Document");
        assert!(!info.is_executable);
        assert!(!info.is_directory);
        assert_eq!(info.size, 5);
        assert!(info.modified_unix.is_some());
        let json = info.to_json();
        assert!(json.contains("\"exists\":true"), "{json}");
        assert!(json.contains("\"size\":5"), "{json}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_info_directory() {
        let dir = temp_dir("dir");
        let info = file_info_local(dir.to_str().unwrap());
        assert!(info.exists);
        assert!(info.is_directory);
        assert_eq!(info.type_name, "Folder");
        assert_eq!(info.size, 0);
        #[cfg(not(windows))]
        assert!(info.attributes.contains(&"directory".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_info_missing_file() {
        let info = file_info_local("/no/such/path/missing.exe");
        assert!(!info.exists);
        assert_eq!(info.display_name, "missing.exe");
        assert_eq!(info.extension, "exe");
        assert!(info.is_executable);
        assert!(!info.is_directory);
        assert!(info.attributes.is_empty());
        assert_eq!(info.size, 0);
        assert_eq!(info.modified_unix, None);
        assert!(info.to_json().contains("\"modified_unix\":null"));
    }

    // -- Verb labels and shaping --

    #[test]
    fn verb_label_accelerators() {
        assert_eq!(parse_verb_label("&Open"), ("Open".into(), Some('O')));
        assert_eq!(
            parse_verb_label("Open &with Editor"),
            ("Open with Editor".into(), Some('w'))
        );
        assert_eq!(parse_verb_label("E&&xit"), ("E&xit".into(), None));
        assert_eq!(parse_verb_label("Plain"), ("Plain".into(), None));
        assert_eq!(parse_verb_label("Trail&"), ("Trail&".into(), None));
        // Only the FIRST marker is the accelerator.
        assert_eq!(
            parse_verb_label("&Open &Here"),
            ("Open Here".into(), Some('O'))
        );
    }

    #[test]
    fn verb_shaping_dedup_and_default() {
        let raw = vec![
            ("open".to_string(), "&Open".to_string()),
            ("OPEN".to_string(), "Duplicate".to_string()),
            ("edit".to_string(), String::new()),
            ("".to_string(), "Nameless".to_string()),
            ("print".to_string(), "&Print".to_string()),
        ];
        let verbs = shape_verbs(raw, Some("EDIT"));
        assert_eq!(verbs.len(), 3);
        assert_eq!(verbs[0].name, "open");
        assert_eq!(verbs[0].label, "Open");
        assert_eq!(verbs[0].accelerator, Some('O'));
        assert!(!verbs[0].is_default);
        // Empty label falls back to the verb name; the
        // default match is case-insensitive.
        assert_eq!(verbs[1].name, "edit");
        assert_eq!(verbs[1].label, "edit");
        assert!(verbs[1].is_default);
        assert_eq!(verbs[2].name, "print");
        let json = verbs_to_json("C:\\a.txt", &verbs);
        assert!(json.contains("\"default_verb\":\"edit\""), "{json}");
        assert!(json.contains("\"accelerator\":\"O\""), "{json}");
        assert!(json.contains("\"accelerator\":null"), "{json}");
    }

    #[test]
    fn verb_shaping_no_default() {
        let verbs = shape_verbs(vec![("open".into(), "Open".into())], None);
        assert!(!verbs[0].is_default);
        let json = verbs_to_json("x", &verbs);
        assert!(json.contains("\"default_verb\":null"), "{json}");
        // A default naming a verb that is not present
        // marks nothing and invents nothing.
        let verbs = shape_verbs(vec![("open".into(), "Open".into())], Some("explore"));
        assert_eq!(verbs.len(), 1);
        assert!(!verbs[0].is_default);
    }

    // -- ShellExecute error mapping --

    #[test]
    fn shell_execute_error_mapping() {
        assert_eq!(shell_execute_error(2).0, ERR_NOT_FOUND);
        assert_eq!(shell_execute_error(3).0, ERR_NOT_FOUND);
        assert_eq!(shell_execute_error(5).0, ERR_ACCESS_DENIED);
        assert_eq!(shell_execute_error(31).0, ERR_NOT_FOUND);
        assert_eq!(shell_execute_error(0).0, ERR_PLATFORM);
        assert_eq!(shell_execute_error(26).0, ERR_PLATFORM);
        assert!(shell_execute_error(2).1.contains("not found"));
    }

    // -- Association / icon JSON shapes --

    #[test]
    fn association_json_shape() {
        let a = Association {
            extension: ".txt".into(),
            prog_id: Some("txtfile".into()),
            app_friendly_name: None,
            open_command: Some("notepad.exe \"%1\"".into()),
        };
        let j = a.to_json();
        assert!(j.contains("\"extension\":\".txt\""), "{j}");
        assert!(j.contains("\"prog_id\":\"txtfile\""), "{j}");
        assert!(j.contains("\"app_friendly_name\":null"), "{j}");
        assert!(
            j.contains("\"open_command\":\"notepad.exe \\\"%1\\\"\""),
            "{j}"
        );
        assert_eq!(
            icon_info_json(-1, false),
            "{\"icon_index\":-1,\"has_icon\":false}"
        );
        assert_eq!(
            icon_info_json(3, true),
            "{\"icon_index\":3,\"has_icon\":true}"
        );
    }
}
