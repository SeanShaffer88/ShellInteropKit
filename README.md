# ShellInteropKit

## TL;DR

A small, zero-dependency DLL that wraps the fiddly parts of the Windows Shell — file info, type names, file associations, context-menu verbs, icons, opening files and the Properties dialog — behind a tiny C ABI that returns plain JSON and plain error codes. No COM boilerplate, no registry walking, no icon handles to remember to destroy: the library handles lifetimes inside the call and hands back values.

```c
#include "shellinteropkit.h"

int main(void) {
    char *info = si_file_info("C:\\docs\\notes.txt");
    printf("%s\n", info);
    /* {"exists":true,"display_name":"notes.txt","extension":"txt",
       "type_name":"Text Document","is_executable":false,
       "is_directory":false,"attributes":["archive"],"size":1234,
       "modified_unix":1761436800} */
    si_free_string(info);

    char *verbs = si_verbs("C:\\docs\\notes.txt");
    printf("%s\n", verbs);   /* the registered verbs, default marked */
    si_free_string(verbs);

    si_open("C:\\docs\\notes.txt");            /* default verb */
    si_show_properties("C:\\docs\\notes.txt"); /* Properties dialog */
    return 0;
}
```

## Features

- **File info as one JSON object** (`si_file_info`): display name, extension, friendly type name, executable / directory flags, attribute names, size and modification time. A missing file is a normal result (`"exists":false`), not an error.
- **Real shell type names on Windows**: `si_file_info` prefers the Shell's own type name (`SHGetFileInfoW` with `SHGFI_TYPENAME`). A built-in fallback table (`.txt` → Text Document, `.png` → PNG Image, about 60 common types) covers the rest, and is also what non-Windows platforms use. The table is a fallback and is documented as one, everywhere it appears.
- **Associations** (`si_association`, Windows): ProgID, friendly application name and open command for an extension, via `AssocQueryStringW`. Accepts `.txt`, `txt`, `file.txt` or a full path; each field is independently nullable because real associations are.
- **Verbs, shaped for display** (`si_verbs`, Windows): the verbs registered for a file or folder, read from the registry (`Directory\shell` for folders, the ProgID's `\shell` key for files, `*\shell` as the fallback). Labels have their `&` accelerator stripped for display with the accelerator character reported separately, names are deduplicated, and the default verb is marked.
- **Shell actions without structs** (`si_open`, `si_invoke_verb`, `si_show_properties`, Windows): all three go through `ShellExecuteW`, which takes plain pointers — no caller-built structures at all. `ShellExecute` result codes are mapped to this library's error codes (file not found, access denied, no association, …) with a plain-language message.
- **Icon facts without handle leaks** (`si_icon_info`, Windows): reports the system icon index and whether the Shell produced an icon. The `HICON` is destroyed inside the call; no handle ever crosses the ABI.
- **Pure helpers that work everywhere**: attribute-bitfield → names (`si_attribute_names`), the fallback type table (`si_type_name_for_extension`), and runnable-type detection (`si_is_executable`).
- **Lifetime-safe by construction**: every returned string is caller-owned and freed with `si_free_string`; every registry key opened is closed on every path out; nothing unwinds across the FFI boundary.

## Building

Rust 1.80+, no dependencies.

```sh
cargo build --release   # shell_interop_kit.dll / .so / .a
cargo test              # full suite (21 tests)
```

The full WinDevKit gate (format, clippy on host and on `x86_64-pc-windows-msvc`, full tests) is `../windevkit/tools/gate.sh`. Note the gate's own caveat: check/clippy do not link, so the real Windows link is exercised by CI's `cargo build --release` on Windows.

## The API in one screen

| Call | Returns | Platforms |
|---|---|---|
| `si_version` / `si_last_error` / `si_last_error_code` / `si_free_string` | borrowed strings / codes | all |
| `si_attribute_names(attrs)` | JSON array of names, caller-owned | all |
| `si_type_name_for_extension(ext)` | fallback type name, caller-owned; NULL if the table has no entry | all |
| `si_is_executable(path_or_ext)` | 1 / 0 / −1 on invalid input | all |
| `si_file_info(path)` | JSON object (see the header for every field rule), caller-owned | all; type name + attributes are shell/Win32-real on Windows |
| `si_association(ext)` | JSON `{extension, prog_id, app_friendly_name, open_command}`, caller-owned | Windows only |
| `si_verbs(path)` | JSON `{path, default_verb, verbs:[{name, label, accelerator, is_default}]}`, caller-owned | Windows only |
| `si_icon_info(path)` | JSON `{icon_index, has_icon}`, caller-owned | Windows only |
| `si_open(path)` | `SI_OK` or an `SI_ERR_*` code | Windows only |
| `si_invoke_verb(path, verb)` | `SI_OK` or an `SI_ERR_*` code | Windows only |
| `si_show_properties(path)` | `SI_OK` or an `SI_ERR_*` code | Windows only |

Windows-only calls made off Windows fail cleanly with `SI_ERR_NOT_SUPPORTED` and a plain message — they never crash and never fake a result.

## Honest limitations

- **Windows runtime is Unverified.** The Windows code paths compile and pass clippy/check for `x86_64-pc-windows-msvc`, and CI does the real link, but v0.1.0 has not yet been run on a real Windows machine. Behaviour claims for the shell calls rest on the documented APIs, not on a field test.
- **No `ShellExecuteExW`.** There is deliberately no wrapper that needs `SHELLEXECUTEINFOW`: that structure's field layout is load-bearing, and shipping a possibly-wrong layout is worse than shipping one fewer function. Consequences: no process handle back from a launch, no `SEE_MASK_*` behaviour. If it is ever added, it goes on the roadmap first and is verified on Windows before it ships.
- **Icons are facts, not pixels.** `si_icon_info` reports the index and presence of an icon; extracting or saving icon images is roadmap, not v0.1.
- **Verb labels are the simple kind.** A verb's label is its registry key's default value. Verbs that name their label indirectly (a `MUIVerb` resource reference, for example) fall back to the verb name rather than resolving the resource string.
- **Verb sources are the classic ones.** Verbs come from `HKCR\*\shell`, `HKCR\Directory\shell` and the file ProgID's `\shell` key. Verbs contributed only through modern shell extension handlers (COM `IContextMenu` implementations) do not appear — they are code, not registry data, and enumerating them is a different feature.
- **The fallback type table is a fallback.** It covers common extensions with the names Windows commonly uses, but the Shell's own table is larger and localised; off Windows (or when the Shell has no name), an unknown extension reports as `<EXT> File`.
- **Attributes off Windows are derived, and only two of them.** `si_file_info` reports `directory` and/or `readonly` from standard metadata; it does not invent the rest of the Win32 set. `si_attribute_names` itself maps the real Win32 bitfield on any platform.

## Tests

21 tests: attribute mapping (including the `NORMAL`-only rule and unknown bits), the type table and its fallback form, the executable-detection matrix, extension/display-name extraction, verb label accelerator parsing (`&Open`, `&&`, trailing `&`), verb shaping (dedup, default marking, empty-label fallback), `ShellExecute` error mapping, `si_file_info` against a real temporary file, a real directory and a missing file, the FFI surface end to end, and — off Windows — every Windows-only call returning `SI_ERR_NOT_SUPPORTED`.

License: MIT. Part of the WinDevKit family — see `AGENTS.md` for build discipline and `ROADMAP.md` for what's next.
