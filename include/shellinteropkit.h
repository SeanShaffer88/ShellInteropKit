/* ShellInteropKit.dll — a typed, lifetime-safe layer over
 * the Windows Shell: file info, associations, verbs, icons
 * and shell actions without raw COM/Shell boilerplate.
 *
 *   char *info = si_file_info("C:\\docs\\notes.txt");
 *   printf("%s\n", info);   // one JSON object, see below
 *   si_free_string(info);
 *   si_open("C:\\docs\\notes.txt");          // default verb
 *   si_show_properties("C:\\docs\\notes.txt");
 *
 * STRINGS AND OWNERSHIP (the lifetime rules, in full).
 * - All strings are UTF-8, NUL-terminated.
 * - Every char* RETURNED by a function below is
 *   caller-owned: free it exactly once with
 *   si_free_string(). NULL means failure — read
 *   si_last_error() / si_last_error_code().
 * - si_version() and si_last_error() return BORROWED
 *   strings: never free them. si_last_error() is
 *   thread-local and stays valid until the next
 *   ShellInteropKit call on the same thread.
 * - No Windows handle ever crosses this ABI. Icon handles
 *   (HICON) are obtained, read and destroyed inside the
 *   call that needed them; registry keys are opened and
 *   closed inside the call. There is nothing to close,
 *   release or destroy on the caller side, ever.
 * - Input pointers are borrowed for the call only and
 *   must be valid UTF-8 (paths may be any UTF-8 path the
 *   OS accepts). NULL inputs are SI_ERR_INVALID_ARG,
 *   never a crash.
 *
 * ERRORS. Action functions return SI_OK (0) or an SI_ERR_*
 * code. String functions return NULL on failure. Either
 * way the thread-local last error carries the code and a
 * plain-language message.
 *
 * PLATFORMS. si_file_info, si_attribute_names,
 * si_type_name_for_extension and si_is_executable work on
 * every platform. si_association, si_verbs, si_icon_info,
 * si_open, si_invoke_verb and si_show_properties are
 * WINDOWS-ONLY: elsewhere they fail with
 * SI_ERR_NOT_SUPPORTED and a plain message.
 */
#ifndef SHELLINTEROPKIT_H
#define SHELLINTEROPKIT_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#if defined(_WIN32) && defined(SI_BUILD)
#define SI_API __declspec(dllexport)
#elif defined(_WIN32)
#define SI_API __declspec(dllimport)
#else
#define SI_API
#endif

enum {
    SI_OK = 0,
    SI_ERR_INVALID_ARG = 1,   /* null pointer, empty path/verb, no extension */
    SI_ERR_NOT_SUPPORTED = 2, /* Windows-only call made off Windows */
    SI_ERR_NOT_FOUND = 3,     /* no such association target / shell info / verb key */
    SI_ERR_ACCESS_DENIED = 4, /* the shell reported access denied (SE_ERR_ACCESSDENIED) */
    SI_ERR_PLATFORM = 5,      /* an OS or shell call failed; see si_last_error() */
    SI_ERR_INTERNAL = 6       /* a panic was caught at the FFI boundary */
};

/* Library version string, e.g. "0.1.0". Borrowed: do not free. */
SI_API const char *si_version(void);

/* Thread-local last-error message. Borrowed: do not free.
 * Empty when the last call on this thread succeeded. */
SI_API const char *si_last_error(void);
SI_API int si_last_error_code(void);

/* Free a caller-owned string returned by this library.
 * NULL is a no-op. Freeing any other pointer, or freeing
 * the same string twice, is a contract violation. */
SI_API void si_free_string(char *p);

/* --- Pure helpers (every platform) --- */

/* Attribute names for a Win32 FILE_ATTRIBUTE_* bitfield,
 * as a JSON array: si_attribute_names(0x21) ->
 * "[\"readonly\",\"archive\"]". FILE_ATTRIBUTE_NORMAL
 * (0x80) is reported only when it is the sole bit set;
 * unknown bits are ignored. Caller-owned result. */
SI_API char *si_attribute_names(uint32_t attributes);

/* The built-in FALLBACK type name for an extension
 * ("png", ".png" and "PNG" are all accepted):
 * si_type_name_for_extension("png") -> "PNG Image".
 * Returns NULL (SI_ERR_NOT_FOUND) when the table has no
 * entry — this is a fallback table, documented as such;
 * si_file_info prefers the shell's own type name on
 * Windows and otherwise uses the "<EXT> File" form.
 * Caller-owned result. */
SI_API char *si_type_name_for_extension(const char *ext);

/* 1 when the argument's extension names a runnable type
 * (.exe .com .bat .cmd .msi .msp .ps1 .lnk .scr .pif
 * .application .gadget .msc .cpl .vbs .vbe .js .jse .wsf
 * .wsh), 0 when it does not, -1 on invalid input
 * (last error SI_ERR_INVALID_ARG). Accepts a full path, a
 * file name, or a bare extension. Runnable says nothing
 * about safe — it is a type fact, not a trust decision. */
SI_API int si_is_executable(const char *path_or_ext);

/* --- File info (every platform) --- */

/* One JSON object describing a path:
 *
 *   {"exists":true,"display_name":"notes.txt",
 *    "extension":"txt","type_name":"Text Document",
 *    "is_executable":false,"is_directory":false,
 *    "attributes":["archive"],"size":1234,
 *    "modified_unix":1761436800}
 *
 * - exists        false for a missing path — that is a
 *                 normal result, NOT an error; the other
 *                 fields are then derived from the path
 *                 text alone, with size 0 and
 *                 modified_unix null.
 * - display_name  the final path component.
 * - extension     lowercase, without the dot; "" if none
 *                 (a leading dot is part of the name).
 * - type_name     on Windows, the shell's own type name
 *                 (SHGetFileInfoW / SHGFI_TYPENAME) when
 *                 the shell provides one, with the
 *                 built-in fallback table behind it;
 *                 elsewhere, the fallback table ("Folder"
 *                 for directories, "<EXT> File" for
 *                 extensions the table does not know).
 * - attributes    attribute NAMES. On Windows these are
 *                 the real Win32 FILE_ATTRIBUTE_* names;
 *                 elsewhere only "directory" and
 *                 "readonly" are derived, from std
 *                 metadata — nothing else is claimed.
 * - size          file size in bytes; 0 for directories
 *                 and missing paths.
 * - modified_unix seconds since the Unix epoch, or null.
 *
 * Caller-owned result; NULL only for invalid arguments. */
SI_API char *si_file_info(const char *path);

/* --- Windows-only queries --- */

/* The file association for an extension, as JSON:
 *
 *   {"extension":".txt","prog_id":"txtfile",
 *    "app_friendly_name":"Notepad",
 *    "open_command":"\"...notepad.exe\" \"%1\""}
 *
 * Accepts ".txt", "txt", "file.txt" or a full path; the
 * extension field echoes the normalized dotted form.
 * Any of prog_id / app_friendly_name / open_command may
 * be null: an extension can legitimately lack any of
 * them. WINDOWS-ONLY (AssocQueryStringW): elsewhere NULL
 * with SI_ERR_NOT_SUPPORTED. Caller-owned result. */
SI_API char *si_association(const char *ext);

/* The shell verbs registered for a file or folder:
 *
 *   {"path":"C:\\a.txt","default_verb":"open",
 *    "verbs":[{"name":"open","label":"Open",
 *              "accelerator":"O","is_default":true}]}
 *
 * Verbs are read from the registry: Directory\shell for
 * folders, the file ProgID's <ProgID>\shell key when a
 * ProgID exists, and *\shell otherwise. A verb's label is
 * its key's default value (the verb name when the value
 * is empty or missing), with the '&' accelerator marker
 * removed for display ("&Open" -> label "Open",
 * accelerator "O"; "&&" displays one literal '&' and
 * marks nothing). Names are deduplicated
 * case-insensitively, first entry wins. default_verb is
 * the name the shell key's own default value names, or
 * null when it names none of the listed verbs.
 * WINDOWS-ONLY: elsewhere NULL with
 * SI_ERR_NOT_SUPPORTED. Caller-owned result. */
SI_API char *si_verbs(const char *path);

/* Icon presence for a path, as JSON:
 *
 *   {"icon_index":3,"has_icon":true}
 *
 * icon_index is the system image-list index the shell
 * reports; has_icon says the shell produced an icon. The
 * HICON behind this is destroyed inside the call — this
 * function reports facts about the icon, it does not
 * hand the icon out, and extracting icon pixels is not
 * supported (see the roadmap). WINDOWS-ONLY: elsewhere
 * NULL with SI_ERR_NOT_SUPPORTED. Caller-owned result. */
SI_API char *si_icon_info(const char *path);

/* --- Windows-only actions (ShellExecuteW) --- */

/* Open a path with its default verb. Returns SI_OK or an
 * SI_ERR_* code mapped from the ShellExecute result
 * (file/path not found -> SI_ERR_NOT_FOUND, access
 * denied -> SI_ERR_ACCESS_DENIED, no association ->
 * SI_ERR_NOT_FOUND). WINDOWS-ONLY: SI_ERR_NOT_SUPPORTED
 * elsewhere. */
SI_API int si_open(const char *path);

/* Invoke a named verb ("open", "edit", "print", ...) on
 * a path. The verb must not be NULL or empty. Return
 * codes as si_open. WINDOWS-ONLY. */
SI_API int si_invoke_verb(const char *path, const char *verb);

/* Show the shell Properties dialog for a path (the
 * "properties" verb). Return codes as si_open.
 * WINDOWS-ONLY. */
SI_API int si_show_properties(const char *path);

#ifdef __cplusplus
}
#endif
#endif /* SHELLINTEROPKIT_H */
