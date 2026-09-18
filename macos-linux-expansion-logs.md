# macOS and Linux Expansion Log

This file records checkpoints, skipped procedures, and validation limits for work on `feat/mac-linux`.

## 2026-09-18 — Core portability slice started

- Confirmed the active branch is `feat/mac-linux` and matches `origin/feat/mac-linux` at `e687b0a` before edits.
- Preserved unrelated untracked files (`BUILDING.md`, `assets/403.html`, `fw-implementation-plan.md`, and `test.ps1`).
- Baseline Windows suite reported by the architecture audit: 98 tests passed.
- Began target-neutral platform and IPC facades while retaining the existing Windows SID plus installation-directory identity.
- Added Unix runtime-scope, lock, and socket implementations with security-focused tests.
- Installed the `x86_64-unknown-linux-gnu` Rust standard library locally, but cross-checking from Windows stopped in `ring` because no `x86_64-linux-gnu-gcc` toolchain is installed. This is an environment boundary rather than a Rust diagnostic; Unix compilation and execution remain assigned to GitHub-hosted runners immediately before the first remote build attempt.

## 2026-09-18 — Remote Unix test run 1

- Dry-run workflow `35357730608` used `version=0.1.1` and `publish=no`.
- Linux and Apple Silicon macOS both compiled the Unix implementation and ran 110 tests.
- Both platforms passed 107 tests and failed the same three Unix-socket tests because synchronous tests called `tokio::net::UnixListener::bind` without a Tokio reactor.
- The production socket code was not weakened. The three tests were corrected to use `#[tokio::test]`, matching the transport's actual runtime requirement.
- Windows compilation was canceled by workflow failure propagation while still compiling dependencies; the preceding local Windows suite and release build had passed.

## 2026-09-18 — Remote Unix test and build run 2

- Dry-run workflow `35358179535` used `version=0.1.2` and `publish=no`.
- Linux native test suite passed all 110 tests.
- Apple Silicon macOS native test suite passed all 110 tests on the `macos-14-arm64` runner.
- Portable Linux musl builds succeeded for AMD64 and ARM64.
- Portable macOS builds succeeded for AMD64 and ARM64.
- The full Windows MSVC build, NSIS packaging, four-file verification, and artifact upload remained green.
- The GitHub release creation step was correctly skipped.
- Experimental Unix build artifacts are uploaded separately and are not yet included in final release contents; setup migration and archive packaging must be completed first.

## 2026-09-18 — Unix setup, lifecycle, and packaging implementation

- Replaced the dependency-heavy Unix setup implementation with a single `fw-setup.sh` asset containing a small POSIX bootstrap and embedded Python standard-library setup source. No Python runtime or second setup asset is bundled.
- The bootstrap never modifies an existing Python installation. When Python is absent, it requires explicit consent, records the exact package it installed in protected per-user state, retains it across failed retries, and removes only that recorded package after successful setup without using autoremove.
- Added TTY- and `NO_COLOR`-aware terminal styling matching the Windows setup's headings, statuses, warning labels, and red-background error labels.
- The Rust setup bootstrap now executes the verified open Unix script descriptor through `/dev/fd/3`, preserving interactive stdin and avoiding a pathname reopen after SHA-256 verification.
- Independently verified the pinned Cloudflared `2026.9.1` Linux binary hashes and macOS archive hashes against GitHub release asset digests. The macOS extracted-binary hashes match the upstream release-note checksums.
- Added Unix Cloudflared process-group supervision with orderly `SIGTERM`, bounded `SIGKILL` escalation, descendant cleanup tests, and normal child reaping. Windows continues to use its existing kill-on-close Job Object behavior.
- Added Unix daemon `SIGTERM` and `SIGINT` handling through the existing root cancellation path, including tests that ensure the signal waiter exits when another shutdown source wins.
- Expanded release packaging to native Linux and macOS test matrices, four verified portable `.tar.gz` archives, exact nine-file release aggregation, and a complete bundle artifact even when `publish=no`.
- Local validation passed: `sh -n`, embedded setup self-test, workflow/file diagnostics, formatting, all 100 Windows GNU tests, and the Windows GNU release build/check.
- Native Unix compilation, process-group tests, archive execution, runner-label availability, and final workflow aggregation still require the next GitHub Actions dry run with `publish=no`.

## Skipped or deferred procedures

- Live Cloudflare OAuth/API/tunnel testing remains intentionally deferred to manual testing; automated tests exercise pure setup logic and packaging without creating real Cloudflare resources.
