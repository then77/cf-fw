# macOS and Linux Expansion Log

This file records checkpoints, skipped procedures, and validation limits for work on `feat/mac-linux`.

## 2026-09-18 — Core portability slice started

- Confirmed the active branch is `feat/mac-linux` and matches `origin/feat/mac-linux` at `e687b0a` before edits.
- Preserved unrelated untracked files (`BUILDING.md`, `assets/403.html`, `fw-implementation-plan.md`, and `test.ps1`).
- Baseline Windows suite reported by the architecture audit: 98 tests passed.
- Began target-neutral platform and IPC facades while retaining the existing Windows SID plus installation-directory identity.
- Added Unix runtime-scope, lock, and socket implementations with security-focused tests.
- Installed the `x86_64-unknown-linux-gnu` Rust standard library locally, but cross-checking from Windows stopped in `ring` because no `x86_64-linux-gnu-gcc` toolchain is installed. This is an environment boundary rather than a Rust diagnostic; Unix compilation and execution remain assigned to GitHub-hosted runners immediately before the first remote build attempt.

## Skipped or deferred procedures

None yet. A procedure will be listed here rather than bypassed if it cannot be implemented or tested safely.
