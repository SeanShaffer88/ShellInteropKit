# ShellInteropKit Roadmap

## v0.1.0 (current)

The typed shell layer behind a C ABI: file info as one JSON object (shell type names on Windows, a documented fallback table elsewhere), associations via `AssocQueryStringW`, registry verbs shaped for display (accelerator parsing, dedup, default marked), icon presence + index with the `HICON` destroyed inside the call, and open / invoke-verb / properties actions via `ShellExecuteW`. Pure helpers (attribute names, type table, executable detection) work and are tested on every platform; the full gate including the windows-msvc clippy/check passes, and the Windows runtime itself is awaiting a first on-machine field test.

## Next, in order

1. **On-Windows field verification**: run the shipped DLL on a real Windows machine against known files (a `.txt` with a standard association, a folder, an executable) and record what the shell actually returns for `si_file_info`, `si_association`, `si_verbs` and `si_icon_info`. Every later item depends on this evidence.
2. **Icon extraction**: return icon pixels (a documented bitmap/PNG byte format) for callers that need to draw the icon themselves. Needs a careful memory story: caller-owned buffers, size limits, and the same "no raw handle escapes" rule.
3. **`ShellExecuteExW`, only if verified**: process handles and `SEE_MASK_*` behaviours (wait-for-exit, no-console, runas elevation verb support) — gated on the `SHELLEXECUTEINFOW` layout being proven on a real Windows build first, not merely believed. Until then `ShellExecuteW` is the action API.
4. **`MUIVerb` label resolution**: resolve indirect verb labels (`@dll,-id` resource references) so verbs that name their label indirectly display their real text instead of falling back to the verb name.
5. **Perceived types and kind names**: expose the shell's perceived-type classification (image / audio / video / document / …) alongside the type name, from the same association data.

## Explicitly not planned

- COM context-menu hosting (`IContextMenu` enumeration/invocation): it requires apartment and message-pump commitments that do not belong inside a stateless C ABI library; apps that need live shell extensions should host them in-process.
- A shell namespace browser (PIDLs, `IShellFolder`): a different, much larger library wearing this one's name.
- Writing associations or verbs (registry mutation): ShellInteropKit reads and invokes; changing the user's associations is an installer's job, with an installer's consent story.
