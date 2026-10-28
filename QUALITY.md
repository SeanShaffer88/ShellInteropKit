# ShellInteropKit — Quality Scorecard

Version scored: 0.1.0 (2026-10-28). Scores are from evidence gathered
on the build machine: the full gate output and the test suite.
Windows runtime behaviour is **Unverified** — the DLL has not yet
been run on a real Windows machine, so no score here claims the
shell calls work there; what is verified is that the Windows code
compiles and passes clippy/check for the windows-msvc target, and
that every pure-logic and off-Windows path is exercised by tests.

| Area | Score | Evidence |
|---|---|---|
| Core logic | 9/10 | Attribute mapping, extension/type-table logic, executable detection, verb label parsing/shaping/dedup and ShellExecute error mapping are pure functions in the library, each covered by dedicated tests. |
| Tests | 9/10 | Full gate run 2026-10-28: format check, clippy (host and Windows target, warnings denied), Windows-target check, and the full test suite all pass — 21 tests. Coverage includes file_info on a real temp file, a real directory and a missing file, plus every Windows-only call returning SI_ERR_NOT_SUPPORTED off Windows. |
| Lifetime safety | 9/10 | No handle crosses the ABI by construction: HICON destroyed and registry keys closed inside src/platform.rs on every path; returned strings are caller-owned with one documented free function; nothing unwinds across the FFI boundary. Scored from code structure, not a Windows run. |
| Documentation | 9/10 | Header documents every export plus the full ownership rules; README has TL;DR, a C example, the API table, and honest limitations including exactly which calls are Windows-only; roadmap names the deferred ShellExecuteExW and icon-extraction work and why. |
| Safety of design choices | 9/10 | No ShellExecuteExW wrapper shipped: its struct layout could not be verified from this machine, so actions use ShellExecuteW instead and the gap is documented rather than hidden. ASSOCSTR constants limited to the three values the kit needs (1, 4, 8). |
| Packaging | 6/10 | Cargo release profile and CI workflow are in the repo from the family skeleton, but no release has been built or published — packaging is unproven until CI runs it. |
| Windows runtime | Unverified | Not run on a Windows machine yet. A field test is required before this can be scored. |

**Overall: 51/60 on scored areas (≈85/100), with Windows runtime
Unverified and no release yet exercised.** Those two gaps — not the
code — are what stand between this version and a confident release.
