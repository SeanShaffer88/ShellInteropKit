//! Platform layer: the only place Windows APIs are touched.
//!
//! Hand-declared bindings, each extern block carrying its
//! `#[link]` import library:
//!
//! - `SHGetFileInfoW`, `ShellExecuteW` — shell32
//! - `AssocQueryStringW` — shlwapi
//! - `RegOpenKeyExW` / `RegEnumKeyExW` / `RegQueryValueExW` /
//!   `RegCloseKey` — advapi32
//! - `DestroyIcon` — user32
//!
//! Lifetime rules enforced here, not left to callers:
//! every `HICON` obtained is destroyed before the function
//! returns; every registry key opened is closed on every
//! path out. No Windows handle ever crosses this module.
//!
//! On non-Windows targets the shell operations return
//! `ERR_NOT_SUPPORTED` with a plain message; the pure
//! logic in the crate root still works everywhere.

use crate::{Association, ERR_NOT_SUPPORTED};

pub type PResult<T> = Result<T, (i32, String)>;

/// Raw verb data from the registry: `(verb name, raw
/// label)` pairs in enumeration order, plus the shell
/// key's default verb name when one is set.
pub type VerbData = (Vec<(String, String)>, Option<String>);

pub fn not_supported<T>(what: &str) -> PResult<T> {
    Err((
        ERR_NOT_SUPPORTED,
        format!("{what} is only available on Windows"),
    ))
}

// ---------------------------------------------------------------------------
// Windows implementation
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    use crate::{Association, ERR_NOT_FOUND, ERR_PLATFORM};

    use super::PResult;

    // -- Wide-string helpers --

    pub fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn from_wide(buf: &[u16]) -> String {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }

    fn platform_err(what: &str, code: i32) -> (i32, String) {
        (ERR_PLATFORM, format!("{what} failed (error {code})"))
    }

    // -- SHGetFileInfoW (shell32) --

    pub const SHGFI_ICON: u32 = 0x0000_0100;
    pub const SHGFI_TYPENAME: u32 = 0x0000_0400;
    pub const SHGFI_SMALLICON: u32 = 0x0000_0001;

    // SHFILEINFOW: field order is the documented layout —
    // HICON, int icon index, DWORD attributes, then the
    // two fixed buffers (MAX_PATH 260 and 80 chars).
    #[repr(C)]
    pub struct ShFileInfoW {
        pub h_icon: *mut c_void,
        pub i_icon: i32,
        pub dw_attributes: u32,
        pub sz_display_name: [u16; 260],
        pub sz_type_name: [u16; 80],
    }

    impl ShFileInfoW {
        pub fn zeroed() -> Self {
            ShFileInfoW {
                h_icon: std::ptr::null_mut(),
                i_icon: 0,
                dw_attributes: 0,
                sz_display_name: [0; 260],
                sz_type_name: [0; 80],
            }
        }
    }

    #[link(name = "shell32")]
    extern "system" {
        fn SHGetFileInfoW(
            psz_path: *const u16,
            dw_file_attributes: u32,
            psfi: *mut ShFileInfoW,
            cb_file_info: u32,
            u_flags: u32,
        ) -> usize;
        fn ShellExecuteW(
            hwnd: *mut c_void,
            lp_operation: *const u16,
            lp_file: *const u16,
            lp_parameters: *const u16,
            lp_directory: *const u16,
            n_show_cmd: i32,
        ) -> isize;
    }

    #[link(name = "user32")]
    extern "system" {
        fn DestroyIcon(h_icon: *mut c_void) -> i32;
    }

    fn sh_get_file_info(path: &str, flags: u32) -> PResult<ShFileInfoW> {
        let wide = to_wide(path);
        let mut info = ShFileInfoW::zeroed();
        // SAFETY: `wide` is a NUL-terminated UTF-16 path;
        // `info` is a correctly laid-out SHFILEINFOW and
        // cb_file_info is its exact size, as the API
        // requires.
        let ret = unsafe {
            SHGetFileInfoW(
                wide.as_ptr(),
                0,
                &mut info,
                std::mem::size_of::<ShFileInfoW>() as u32,
                flags,
            )
        };
        if ret == 0 {
            return Err((
                ERR_NOT_FOUND,
                format!("the shell returned no information for \"{path}\""),
            ));
        }
        Ok(info)
    }

    /// The shell's own type name for a path, when it has
    /// one (`SHGFI_TYPENAME`).
    pub fn shell_type_name(path: &str) -> PResult<Option<String>> {
        let info = sh_get_file_info(path, SHGFI_TYPENAME)?;
        let name = from_wide(&info.sz_type_name);
        Ok(if name.is_empty() { None } else { Some(name) })
    }

    /// Icon presence + system icon index. The HICON is
    /// destroyed before returning — only plain values
    /// leave this function.
    pub fn icon_info(path: &str) -> PResult<(i32, bool)> {
        let info = sh_get_file_info(path, SHGFI_ICON | SHGFI_SMALLICON)?;
        let has_icon = !info.h_icon.is_null();
        if has_icon {
            // SAFETY: h_icon came from SHGetFileInfoW with
            // SHGFI_ICON in this call; ownership passed to
            // us, and DestroyIcon is its documented release.
            unsafe { DestroyIcon(info.h_icon) };
        }
        Ok((info.i_icon, has_icon))
    }

    const SW_SHOWNORMAL: i32 = 1;

    /// ShellExecuteW: returns a value > 32 on success, or
    /// a SE_ERR_* / Win32 code (<= 32) on failure, which
    /// the caller maps through `shell_execute_error`.
    pub fn shell_execute(path: &str, verb: Option<&str>) -> Result<(), usize> {
        let file = to_wide(path);
        let op = verb.map(to_wide);
        // SAFETY: all pointers are to NUL-terminated wide
        // strings that outlive the call (or null, which the
        // API accepts for lpOperation/lpParameters/
        // lpDirectory); hwnd is null (no owner window).
        let ret = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                op.as_ref().map_or(std::ptr::null(), |v| v.as_ptr()),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if ret > 32 {
            Ok(())
        } else {
            Err(ret as usize)
        }
    }

    // -- AssocQueryStringW (shlwapi) --

    // ASSOCSTR values (shlwapi.h): COMMAND = 1,
    // FRIENDLYAPPNAME = 4, PROGID = 8. Only these three
    // are used — they are the ones this library needs.
    const ASSOCSTR_COMMAND: u32 = 1;
    const ASSOCSTR_FRIENDLYAPPNAME: u32 = 4;
    const ASSOCSTR_PROGID: u32 = 8;

    #[link(name = "shlwapi")]
    extern "system" {
        fn AssocQueryStringW(
            flags: u32,
            assoc_str: u32,
            psz_assoc: *const u16,
            psz_extra: *const u16,
            psz_out: *mut u16,
            pcch_out: *mut u32,
        ) -> i32;
    }

    fn assoc_query(ext_wide: &[u16], assoc_str: u32, extra: Option<&str>) -> Option<String> {
        let extra_wide = extra.map(to_wide);
        let extra_ptr = extra_wide.as_ref().map_or(std::ptr::null(), |v| v.as_ptr());
        // First pass with a generous buffer; if the shell
        // reports a larger required size, grow and retry
        // once. A failed query is None, not an error: an
        // extension can legitimately have no ProgID, no
        // friendly name, or no command.
        let mut size: u32 = 1024;
        let mut buf = vec![0u16; size as usize];
        // SAFETY: ext_wide / extra are NUL-terminated;
        // buf has exactly `size` elements and pcch_out
        // points at the size variable, per the API.
        let hr = unsafe {
            AssocQueryStringW(
                0,
                assoc_str,
                ext_wide.as_ptr(),
                extra_ptr,
                buf.as_mut_ptr(),
                &mut size,
            )
        };
        if hr != 0 {
            if size <= 1024 || size > 32768 {
                return None;
            }
            buf = vec![0u16; size as usize];
            let mut size2 = size;
            // SAFETY: same contract as the first call,
            // with the shell-reported buffer size.
            let hr2 = unsafe {
                AssocQueryStringW(
                    0,
                    assoc_str,
                    ext_wide.as_ptr(),
                    extra_ptr,
                    buf.as_mut_ptr(),
                    &mut size2,
                )
            };
            if hr2 != 0 {
                return None;
            }
        }
        let text = from_wide(&buf);
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }

    pub fn association(ext_dotted: &str) -> PResult<Association> {
        let wide = to_wide(ext_dotted);
        Ok(Association {
            extension: ext_dotted.to_string(),
            prog_id: assoc_query(&wide, ASSOCSTR_PROGID, None),
            app_friendly_name: assoc_query(&wide, ASSOCSTR_FRIENDLYAPPNAME, None),
            open_command: assoc_query(&wide, ASSOCSTR_COMMAND, Some("open")),
        })
    }

    // -- Registry verb enumeration (advapi32) --

    type Hkey = *mut c_void;
    const HKEY_CLASSES_ROOT: usize = 0x8000_0000;
    const KEY_READ: u32 = 0x0002_0019;
    const ERROR_SUCCESS: i32 = 0;
    const ERROR_NO_MORE_ITEMS: i32 = 259;
    const REG_SZ: u32 = 1;
    const REG_EXPAND_SZ: u32 = 2;

    #[link(name = "advapi32")]
    extern "system" {
        fn RegOpenKeyExW(
            h_key: Hkey,
            lp_sub_key: *const u16,
            ul_options: u32,
            sam_desired: u32,
            phk_result: *mut Hkey,
        ) -> i32;
        fn RegEnumKeyExW(
            h_key: Hkey,
            dw_index: u32,
            lp_name: *mut u16,
            lpcch_name: *mut u32,
            lp_reserved: *mut u32,
            lp_class: *mut u16,
            lpcch_class: *mut u32,
            lp_ft_last_write_time: *mut c_void,
        ) -> i32;
        fn RegQueryValueExW(
            h_key: Hkey,
            lp_value_name: *const u16,
            lp_reserved: *mut u32,
            lp_type: *mut u32,
            lp_data: *mut u8,
            lpcb_data: *mut u32,
        ) -> i32;
        fn RegCloseKey(h_key: Hkey) -> i32;
    }

    fn hkcr() -> Hkey {
        HKEY_CLASSES_ROOT as Hkey
    }

    /// Open a key under HKCR. The caller owns the handle
    /// and must close it with RegCloseKey.
    fn reg_open(sub_key: &str) -> Option<Hkey> {
        let wide = to_wide(sub_key);
        let mut handle: Hkey = std::ptr::null_mut();
        // SAFETY: sub_key is NUL-terminated; handle is an
        // out pointer the API fills on success.
        let rc = unsafe { RegOpenKeyExW(hkcr(), wide.as_ptr(), 0, KEY_READ, &mut handle) };
        if rc == ERROR_SUCCESS && !handle.is_null() {
            Some(handle)
        } else {
            None
        }
    }

    fn reg_close(handle: Hkey) {
        if !handle.is_null() {
            // SAFETY: handle came from RegOpenKeyExW and is
            // closed exactly once by its owner.
            unsafe { RegCloseKey(handle) };
        }
    }

    /// Read a string value (the default value when
    /// `value_name` is None). Non-string values and
    /// missing values are None.
    fn reg_read_string(handle: Hkey, value_name: Option<&str>) -> Option<String> {
        let name_wide = value_name.map(to_wide);
        let name_ptr = name_wide.as_ref().map_or(std::ptr::null(), |v| v.as_ptr());
        let mut value_type: u32 = 0;
        let mut size: u32 = 0;
        // SAFETY (both calls): a null data pointer with a
        // size out-pointer is the documented size query;
        // the second call's buffer has exactly the
        // reported byte size.
        let rc = unsafe {
            RegQueryValueExW(
                handle,
                name_ptr,
                std::ptr::null_mut(),
                &mut value_type,
                std::ptr::null_mut(),
                &mut size,
            )
        };
        if rc != ERROR_SUCCESS || size == 0 || (value_type != REG_SZ && value_type != REG_EXPAND_SZ)
        {
            return None;
        }
        let mut bytes = vec![0u8; size as usize];
        let mut size2 = size;
        let rc = unsafe {
            RegQueryValueExW(
                handle,
                name_ptr,
                std::ptr::null_mut(),
                &mut value_type,
                bytes.as_mut_ptr(),
                &mut size2,
            )
        };
        if rc != ERROR_SUCCESS {
            return None;
        }
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let text = from_wide(&units);
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }

    /// Enumerate `(verb name, raw label)` pairs plus the
    /// shell key's default verb name. Every key opened
    /// here is closed here, on every path.
    pub fn enumerate_verbs(shell_key_path: &str) -> PResult<super::VerbData> {
        let shell_key = reg_open(shell_key_path).ok_or_else(|| {
            (
                ERR_NOT_FOUND,
                format!("no shell verbs are registered at \"{shell_key_path}\""),
            )
        })?;
        let default_verb = reg_read_string(shell_key, None);
        let mut out: Vec<(String, String)> = Vec::new();
        let mut index: u32 = 0;
        loop {
            let mut name_buf = vec![0u16; 256];
            let mut name_len: u32 = name_buf.len() as u32;
            // SAFETY: name_buf has 256 elements and
            // name_len is its capacity on entry, per the
            // API; null class / filetime are accepted.
            let rc = unsafe {
                RegEnumKeyExW(
                    shell_key,
                    index,
                    name_buf.as_mut_ptr(),
                    &mut name_len,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if rc == ERROR_NO_MORE_ITEMS {
                break;
            }
            if rc != ERROR_SUCCESS {
                reg_close(shell_key);
                return Err(platform_err("RegEnumKeyExW", rc));
            }
            let verb_name = from_wide(&name_buf);
            index += 1;
            if verb_name.is_empty() {
                continue;
            }
            let label = match reg_open(&format!("{shell_key_path}\\{verb_name}")) {
                Some(sub) => {
                    let l = reg_read_string(sub, None);
                    reg_close(sub);
                    l
                }
                None => None,
            };
            out.push((verb_name, label.unwrap_or_default()));
        }
        reg_close(shell_key);
        Ok((out, default_verb))
    }

    pub fn prog_id_for_path(path: &str) -> Option<String> {
        let ext = crate::extension_of(path);
        if ext.is_empty() {
            return None;
        }
        let dotted = format!(".{ext}");
        let wide = to_wide(&dotted);
        assoc_query(&wide, ASSOCSTR_PROGID, None)
    }
}

// ---------------------------------------------------------------------------
// Public platform surface
// ---------------------------------------------------------------------------

/// The shell's type name for `path` (Windows only). Off
/// Windows this returns `Ok(None)` so the caller falls
/// back to the built-in type table.
#[cfg(windows)]
pub fn shell_type_name(path: &str) -> PResult<Option<String>> {
    win::shell_type_name(path)
}

#[cfg(not(windows))]
pub fn shell_type_name(_path: &str) -> PResult<Option<String>> {
    Ok(None)
}

/// Association data for a dotted extension (Windows only).
#[cfg(windows)]
pub fn association(ext_dotted: &str) -> PResult<Association> {
    win::association(ext_dotted)
}

#[cfg(not(windows))]
pub fn association(_ext_dotted: &str) -> PResult<Association> {
    not_supported("file associations")
}

/// Raw verb pairs + default verb name for `path`
/// (Windows only): directories use `Directory\shell`,
/// files use their ProgID's `\shell` key when a ProgID
/// exists and `*\shell` otherwise.
#[cfg(windows)]
pub fn verbs_for_path(path: &str, is_directory: bool) -> PResult<VerbData> {
    let key = if is_directory {
        "Directory\\shell".to_string()
    } else {
        match win::prog_id_for_path(path) {
            Some(prog_id) => format!("{prog_id}\\shell"),
            None => "*\\shell".to_string(),
        }
    };
    win::enumerate_verbs(&key)
}

#[cfg(not(windows))]
pub fn verbs_for_path(_path: &str, _is_directory: bool) -> PResult<VerbData> {
    not_supported("shell verbs")
}

/// Run a ShellExecuteW operation (Windows only). Success
/// is `Ok(())`; failure carries the raw <= 32 result for
/// mapping through `crate::shell_execute_error`.
#[cfg(windows)]
pub fn shell_execute(path: &str, verb: Option<&str>) -> Result<(), usize> {
    win::shell_execute(path, verb)
}

#[cfg(not(windows))]
pub fn shell_execute(_path: &str, _verb: Option<&str>) -> Result<(), usize> {
    // The non-Windows FFI layer never reaches this: it
    // returns ERR_NOT_SUPPORTED first. This body exists
    // only to keep the signature total.
    Err(usize::MAX)
}

/// Icon index + presence for `path` (Windows only). The
/// icon handle is destroyed inside the platform call.
#[cfg(windows)]
pub fn icon_info(path: &str) -> PResult<(i32, bool)> {
    win::icon_info(path)
}

#[cfg(not(windows))]
pub fn icon_info(_path: &str) -> PResult<(i32, bool)> {
    not_supported("shell icons")
}
