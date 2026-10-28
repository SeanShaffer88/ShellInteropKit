# ShellInteropKit — agent notes

- No handle crosses the ABI, ever: `HICON`s are destroyed and registry keys are closed inside `src/platform.rs` before any function returns. A change that hands a handle to the caller breaks the kit's central promise — redesign the result as plain values instead.
- No `ShellExecuteExW` / `SHELLEXECUTEINFOW` wrapper without on-Windows verification of the exact struct layout (field order, `cbSize`, pointer sizes). The shipped actions use `ShellExecuteW` precisely because it needs no caller-built struct. A wrong layout is worse than one fewer function; the roadmap entry is the only route back.
- `ASSOCSTR` values in use are COMMAND = 1, FRIENDLYAPPNAME = 4, PROGID = 8 — the ones this kit needs. Do not add another `ASSOCSTR_*` constant from memory; verify it against the official `shlwapi.h` documentation first, or drop it.
- The type-name table in `src/lib.rs` is a FALLBACK and must stay labelled as one in every doc that mentions it. On Windows the shell's `SHGFI_TYPENAME` name wins whenever it exists; do not let the table shadow it, and do not grow the table to imitate the shell's full (localised) table.
- Verb labels: a verb's label is its key's default value, with `MUIVerb`-style indirection deliberately unresolved (falls back to the verb name). Do not start resolving resource strings without a roadmap decision — `@dll,-id` handling is a feature, not a fix.
- `FILE_ATTRIBUTE_NORMAL` is reported only when it is the sole bit set — that is its documented meaning. A "simplification" that reports it whenever the bit is set misreports almost every real file.
- Off Windows, the Windows-only calls return `SI_ERR_NOT_SUPPORTED` with a plain message and `si_file_info` derives at most `directory`/`readonly` attributes. Never fake shell data off Windows to make a test prettier.
- Zero dependencies; Win32 bindings are hand-declared in `src/platform.rs` with an explicit `#[link]` on every extern block (shell32 / shlwapi / advapi32 / user32). Check/clippy do not link — CI's real Windows build is the link gate.
- Gate: WinDevKit `tools/gate.sh` (fmt + clippy both targets + full tests).
