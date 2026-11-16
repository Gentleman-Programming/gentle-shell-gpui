# Voice dictation integration tracker

**DRAFT — tracking only; do not merge.** This document gives the tracker branch a reviewable diff while the dependent source slices are reviewed. No PR is opened by this document.

Chain: `main` → `feat/voice-dictation` (tracker) → 13 ordered child branches. Child 1 is based on the original base and targets this documentation-only tracker; its merge-base diff contains core only. Later children target their immediate predecessors. This document is intentionally not part of the application-source equality check.

## Scope

The implemented scope includes externally supplied local Whisper and Vosk engines, a configurable provider, and dictation Settings and guidance. There is no bundled runtime or model and no model-download UI. Transcribed text remains an editable draft for manual sending.

## Verification and limitations

- Parent job 7 passed activated Windows formatting, clippy and workspace tests, exit 0, with the full application tree equal to `792976b`; integration commit `7fb0e24` preserves that tree.
- Parent job 4 reported Python suites of 30, 28, 27 (one skipped), and 9 tests. Linux and macOS CI have not run yet.
- No new GUI, microphone, model inference or provider test is claimed. Settings save persistence was checked structurally.
- Parent job 8 passed Windows fmt, clippy and workspace tests before each 12/13 commit; the launcher suite ran 27 tests (one skipped), and all 9 profiler tests passed.
- Native reviews of 12 and 13 were approved and acknowledged with authority burned; their nonblocking warning claims remain uncorrected. The sandbox may inherit provider environment secrets and has disputed link-boundary checks; the profiler has sampling, malformed-input and timeout-validation warnings. Earlier native advisories remain; no corrections were opened.
- Platform CI and an isolated smoke test remain outstanding. This tracker is not a merge-readiness claim.
