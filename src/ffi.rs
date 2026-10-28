//! C ABI surface — everything declared in
//! include/shellinteropkit.h. Conventions (house standard):
//! - `*mut c_char` results are caller-owned; free with
//!   `si_free_string`.
//! - Failures return null (string results) or an `SI_ERR_*`
//!   code (action results) and set a thread-local
//!   last-error message readable via `si_last_error`.
//! - Nothing unwinds across the FFI boundary
//!   (catch_unwind everywhere; the release profile also
//!   sets panic = "abort").
//! - No Windows handle ever crosses this boundary: icon
//!   handles are destroyed and registry keys are closed
//!   inside the platform layer before any call returns.

// The exported functions ARE the raw-pointer C ABI declared
// in include/shellinteropkit.h; pointer validity is the
// documented contract.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::panic::catch_unwind;

use crate::{
    attribute_names, file_info_local, icon_info_json, normalize_extension, platform, shape_verbs,
    shell_execute_error, type_name_for_extension, verbs_to_json, SiResult, ERR_INTERNAL,
    ERR_INVALID_ARG, ERR_NOT_SUPPORTED, SI_OK,
};

thread_local! {
    static LAST_ERROR: RefCell<(i32, CString)> =
        RefCell::new((0, CString::new("").unwrap()));
}

pub fn set_error(code: i32, msg: &str) {
    let clean: String = msg.chars().filter(|&c| c != '\0').collect();
    LAST_ERROR.with(|e| {
        *e.borrow_mut() = (
            code,
            CString::new(clean).unwrap_or_else(|_| CString::new("error").unwrap()),
        );
    });
}

fn clear_error() {
    set_error(SI_OK, "");
}

fn fail(code: i32, msg: &str) -> c_int {
    set_error(code, msg);
    code as c_int
}

fn panic_code() -> c_int {
    set_error(ERR_INTERNAL, "internal panic caught at the FFI boundary");
    ERR_INTERNAL as c_int
}

fn guard<T, F: FnOnce() -> T>(f: F) -> std::thread::Result<T> {
    catch_unwind(std::panic::AssertUnwindSafe(f))
}

fn cstr<'a>(ptr: *const c_char, what: &str) -> SiResult<&'a str> {
    if ptr.is_null() {
        return Err((ERR_INVALID_ARG, format!("{what} must not be null")));
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .map_err(|_| (ERR_INVALID_ARG, format!("{what} must be valid UTF-8")))
}

/// Deliver a caller-owned string result on success, or
/// set the last error and return null.
fn string_result(result: SiResult<String>) -> *mut c_char {
    match result {
        Ok(text) => match CString::new(text) {
            Ok(s) => {
                clear_error();
                s.into_raw()
            }
            Err(_) => {
                set_error(ERR_INTERNAL, "result string contained a NUL byte");
                std::ptr::null_mut()
            }
        },
        Err((code, msg)) => {
            set_error(code, &msg);
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn si_version() -> *const c_char {
    static VERSION_C: &[u8] = b"0.1.0\0";
    VERSION_C.as_ptr() as *const c_char
}

#[no_mangle]
pub extern "C" fn si_last_error() -> *const c_char {
    LAST_ERROR.with(|e| e.borrow().1.as_ptr())
}

#[no_mangle]
pub extern "C" fn si_last_error_code() -> c_int {
    LAST_ERROR.with(|e| e.borrow().0) as c_int
}

/// # Safety (caller contract): `p` is null or a string this
/// library returned and has not been freed yet. Documented
/// in the header.
#[no_mangle]
pub extern "C" fn si_free_string(p: *mut c_char) {
    if !p.is_null() {
        unsafe {
            drop(CString::from_raw(p));
        }
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (work on every platform)
// ---------------------------------------------------------------------------

/// Attribute names for a Win32 attribute bitfield, as a
/// JSON array of strings. Never fails except on a null-
/// class internal error; an empty bitfield gives `[]`.
#[no_mangle]
pub extern "C" fn si_attribute_names(attributes: u32) -> *mut c_char {
    match guard(|| {
        let names: Vec<String> = attribute_names(attributes)
            .iter()
            .map(|n| format!("\"{n}\""))
            .collect();
        string_result(Ok(format!("[{}]", names.join(","))))
    }) {
        Ok(p) => p,
        Err(_) => {
            panic_code();
            std::ptr::null_mut()
        }
    }
}

/// The built-in fallback type name for an extension, or
/// null when the table has no entry (use si_file_info for
/// the displayable `<EXT> File` fallback form).
#[no_mangle]
pub extern "C" fn si_type_name_for_extension(ext: *const c_char) -> *mut c_char {
    match guard(|| {
        let inner = (|| -> SiResult<String> {
            let ext = cstr(ext, "ext")?;
            match type_name_for_extension(ext) {
                Some(n) => Ok(n.to_string()),
                None => Err((
                    crate::ERR_NOT_FOUND,
                    format!("no built-in type name for extension \"{ext}\""),
                )),
            }
        })();
        string_result(inner)
    }) {
        Ok(p) => p,
        Err(_) => {
            panic_code();
            std::ptr::null_mut()
        }
    }
}

/// 1 when the extension (or the extension of the given
/// path) names a runnable type, 0 when it does not, and
/// -1 on invalid input (null / non-UTF-8), with the
/// last-error channel set to SI_ERR_INVALID_ARG.
#[no_mangle]
pub extern "C" fn si_is_executable(path_or_ext: *const c_char) -> c_int {
    match guard(|| -> c_int {
        match cstr(path_or_ext, "path_or_ext") {
            Ok(s) => {
                clear_error();
                if crate::is_executable_path_or_ext(s) {
                    1
                } else {
                    0
                }
            }
            Err((code, msg)) => {
                set_error(code, &msg);
                -1
            }
        }
    }) {
        Ok(v) => v,
        Err(_) => panic_code(),
    }
}

// ---------------------------------------------------------------------------
// File info (works everywhere; shell-refined on Windows)
// ---------------------------------------------------------------------------

fn file_info_impl(path: *const c_char) -> SiResult<String> {
    let path = cstr(path, "path")?;
    if path.is_empty() {
        return Err((ERR_INVALID_ARG, "path must not be empty".to_string()));
    }
    let mut info = file_info_local(path);
    if info.exists {
        // On Windows prefer the shell's own type name;
        // off Windows this is Ok(None) and the fallback
        // table name already in `info` stands.
        if let Ok(Some(shell_name)) = platform::shell_type_name(path) {
            if !info.is_directory {
                info.type_name = shell_name;
            }
        }
    }
    Ok(info.to_json())
}

#[no_mangle]
pub extern "C" fn si_file_info(path: *const c_char) -> *mut c_char {
    match guard(|| string_result(file_info_impl(path))) {
        Ok(p) => p,
        Err(_) => {
            panic_code();
            std::ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Windows-only queries
// ---------------------------------------------------------------------------

fn association_impl(ext: *const c_char) -> SiResult<String> {
    let ext = cstr(ext, "ext")?;
    let dotted = normalize_extension(ext).ok_or((
        ERR_INVALID_ARG,
        format!("\"{ext}\" does not contain an extension"),
    ))?;
    let assoc = platform::association(&dotted)?;
    Ok(assoc.to_json())
}

#[no_mangle]
pub extern "C" fn si_association(ext: *const c_char) -> *mut c_char {
    match guard(|| string_result(association_impl(ext))) {
        Ok(p) => p,
        Err(_) => {
            panic_code();
            std::ptr::null_mut()
        }
    }
}

fn verbs_impl(path: *const c_char) -> SiResult<String> {
    let path = cstr(path, "path")?;
    if path.is_empty() {
        return Err((ERR_INVALID_ARG, "path must not be empty".to_string()));
    }
    let is_dir = std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false);
    let (raw, default_name) = platform::verbs_for_path(path, is_dir)?;
    let verbs = shape_verbs(raw, default_name.as_deref());
    Ok(verbs_to_json(path, &verbs))
}

#[no_mangle]
pub extern "C" fn si_verbs(path: *const c_char) -> *mut c_char {
    match guard(|| string_result(verbs_impl(path))) {
        Ok(p) => p,
        Err(_) => {
            panic_code();
            std::ptr::null_mut()
        }
    }
}

fn icon_info_impl(path: *const c_char) -> SiResult<String> {
    let path = cstr(path, "path")?;
    if path.is_empty() {
        return Err((ERR_INVALID_ARG, "path must not be empty".to_string()));
    }
    let (index, has_icon) = platform::icon_info(path)?;
    Ok(icon_info_json(index, has_icon))
}

#[no_mangle]
pub extern "C" fn si_icon_info(path: *const c_char) -> *mut c_char {
    match guard(|| string_result(icon_info_impl(path))) {
        Ok(p) => p,
        Err(_) => {
            panic_code();
            std::ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Windows-only actions (ShellExecuteW; no structs)
// ---------------------------------------------------------------------------

fn shell_action(path: *const c_char, verb: Option<&str>, what: &str) -> c_int {
    match guard(|| -> c_int {
        let inner = (|| -> SiResult<()> {
            let path = cstr(path, "path")?;
            if path.is_empty() {
                return Err((ERR_INVALID_ARG, "path must not be empty".to_string()));
            }
            if cfg!(not(windows)) {
                return Err((
                    ERR_NOT_SUPPORTED,
                    format!("{what} is only available on Windows"),
                ));
            }
            platform::shell_execute(path, verb).map_err(shell_execute_error)
        })();
        match inner {
            Ok(()) => {
                clear_error();
                SI_OK as c_int
            }
            Err((code, msg)) => fail(code, &msg),
        }
    }) {
        Ok(v) => v,
        Err(_) => panic_code(),
    }
}

#[no_mangle]
pub extern "C" fn si_open(path: *const c_char) -> c_int {
    shell_action(path, None, "opening a file through the shell")
}

#[no_mangle]
pub extern "C" fn si_invoke_verb(path: *const c_char, verb: *const c_char) -> c_int {
    match guard(|| -> c_int {
        match cstr(verb, "verb") {
            Ok(v) if !v.is_empty() => shell_action(path, Some(v), "invoking a shell verb"),
            Ok(_) => fail(ERR_INVALID_ARG, "verb must not be empty"),
            Err((code, msg)) => fail(code, &msg),
        }
    }) {
        Ok(v) => v,
        Err(_) => panic_code(),
    }
}

#[no_mangle]
pub extern "C" fn si_show_properties(path: *const c_char) -> c_int {
    shell_action(path, Some("properties"), "showing shell properties")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cs(s: &str) -> CString {
        CString::new(s).unwrap()
    }

    fn take_string(p: *mut c_char) -> String {
        assert!(!p.is_null());
        let s = unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_string();
        si_free_string(p);
        s
    }

    #[test]
    fn ffi_file_info_temp_file() {
        let dir = std::env::temp_dir().join(format!("si-ffi-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("demo.txt");
        std::fs::write(&file, b"abc").unwrap();
        let p = si_file_info(cs(file.to_str().unwrap()).as_ptr());
        let json = take_string(p);
        assert!(json.contains("\"exists\":true"), "{json}");
        assert!(json.contains("\"display_name\":\"demo.txt\""), "{json}");
        assert!(json.contains("\"extension\":\"txt\""), "{json}");
        assert!(json.contains("\"size\":3"), "{json}");
        assert_eq!(si_last_error_code(), SI_OK as c_int);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ffi_file_info_missing_and_bad_args() {
        let p = si_file_info(cs("/no/such/file.bin").as_ptr());
        let json = take_string(p);
        assert!(json.contains("\"exists\":false"), "{json}");
        // A missing file is a result, not an error.
        assert!(si_file_info(std::ptr::null()).is_null());
        assert_eq!(si_last_error_code(), ERR_INVALID_ARG as c_int);
        assert!(si_file_info(cs("").as_ptr()).is_null());
        assert_eq!(si_last_error_code(), ERR_INVALID_ARG as c_int);
    }

    #[test]
    fn ffi_pure_helpers() {
        let p = si_attribute_names(crate::FILE_ATTRIBUTE_HIDDEN | crate::FILE_ATTRIBUTE_READONLY);
        assert_eq!(take_string(p), "[\"readonly\",\"hidden\"]");
        assert_eq!(take_string(si_attribute_names(0)), "[]");
        let p = si_type_name_for_extension(cs("png").as_ptr());
        assert_eq!(take_string(p), "PNG Image");
        assert!(si_type_name_for_extension(cs("qqzz").as_ptr()).is_null());
        assert_eq!(si_last_error_code(), crate::ERR_NOT_FOUND as c_int);
        assert_eq!(si_is_executable(cs("setup.exe").as_ptr()), 1);
        assert_eq!(si_is_executable(cs(".bat").as_ptr()), 1);
        assert_eq!(si_is_executable(cs("notes.txt").as_ptr()), 0);
        assert_eq!(si_is_executable(cs("txt").as_ptr()), 0);
        assert_eq!(si_is_executable(std::ptr::null()), -1);
        assert_eq!(si_last_error_code(), ERR_INVALID_ARG as c_int);
    }

    #[cfg(not(windows))]
    #[test]
    fn ffi_windows_only_calls_are_not_supported() {
        assert!(si_association(cs(".txt").as_ptr()).is_null());
        assert_eq!(si_last_error_code(), ERR_NOT_SUPPORTED as c_int);
        assert!(si_last_error_code() != SI_OK as c_int);
        let msg = unsafe { CStr::from_ptr(si_last_error()) }
            .to_str()
            .unwrap()
            .to_string();
        assert!(msg.contains("Windows"), "{msg}");

        assert!(si_verbs(cs("/tmp").as_ptr()).is_null());
        assert_eq!(si_last_error_code(), ERR_NOT_SUPPORTED as c_int);

        assert!(si_icon_info(cs("/tmp/x.txt").as_ptr()).is_null());
        assert_eq!(si_last_error_code(), ERR_NOT_SUPPORTED as c_int);

        assert_eq!(
            si_open(cs("/tmp/x.txt").as_ptr()),
            ERR_NOT_SUPPORTED as c_int
        );
        assert_eq!(
            si_show_properties(cs("/tmp/x.txt").as_ptr()),
            ERR_NOT_SUPPORTED as c_int
        );
        assert_eq!(
            si_invoke_verb(cs("/tmp/x.txt").as_ptr(), cs("open").as_ptr()),
            ERR_NOT_SUPPORTED as c_int
        );
    }

    #[test]
    fn ffi_invoke_verb_bad_args() {
        assert_eq!(
            si_invoke_verb(cs("/tmp/x.txt").as_ptr(), std::ptr::null()),
            ERR_INVALID_ARG as c_int
        );
        assert_eq!(
            si_invoke_verb(cs("/tmp/x.txt").as_ptr(), cs("").as_ptr()),
            ERR_INVALID_ARG as c_int
        );
        assert_eq!(si_open(std::ptr::null()), ERR_INVALID_ARG as c_int);
    }
}
