# ShellInteropKit — Platform parity

Scored from the code (library, platform layer, FFI) and the test
suite at version 0.1.0. ShellInteropKit is a Windows library by
purpose; this table exists so the off-Windows behaviour is stated
exactly rather than discovered. Off Windows, nothing fakes a shell
result: pure logic works in full, and every shell call fails
cleanly with `SI_ERR_NOT_SUPPORTED` and a plain message.

| Capability | Windows | Other platforms | Notes |
|---|---|---|---|
| `si_version` / `si_last_error` / `si_free_string` | Yes | Yes | Same contract everywhere. |
| `si_attribute_names` | Yes | Yes | Pure bitfield mapping; works anywhere. |
| `si_type_name_for_extension` | Yes | Yes | The built-in fallback table, identical everywhere. |
| `si_is_executable` | Yes | Yes | Pure extension check. |
| `si_file_info` — existence, name, extension, size, modified time | Yes | Yes | From standard filesystem metadata on both. |
| `si_file_info` — type name | Shell's own name first, fallback table behind it | Fallback table only | Windows: `SHGetFileInfoW` / `SHGFI_TYPENAME`. |
| `si_file_info` — attributes | Real Win32 `FILE_ATTRIBUTE_*` names | `directory` / `readonly` only, derived | The smaller off-Windows set is documented, not hidden. |
| `si_association` | Yes (`AssocQueryStringW`) | No — `SI_ERR_NOT_SUPPORTED` | |
| `si_verbs` | Yes (registry enumeration) | No — `SI_ERR_NOT_SUPPORTED` | Verb shaping/parsing itself is pure and tested everywhere. |
| `si_icon_info` | Yes (`SHGetFileInfoW`, icon destroyed in-call) | No — `SI_ERR_NOT_SUPPORTED` | |
| `si_open` / `si_invoke_verb` / `si_show_properties` | Yes (`ShellExecuteW`) | No — `SI_ERR_NOT_SUPPORTED` | |

**Parity summary:** the descriptive half of the kit (what a file
is) works everywhere, with two named quality differences on
Windows (real shell type names, real attribute sets). The shell
half (associations, verbs, icons, actions) is Windows-only by
nature and says so in its error, its header, and this table.
