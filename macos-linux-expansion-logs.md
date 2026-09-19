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

## 2026-09-18 — Focused ARM runner diagnostics

- Dry-run workflow `35369954169` used `version=0.1.7` and `publish=no`; Windows and Intel macOS were temporarily disabled to isolate Linux and Apple Silicon macOS.
- Linux AMD64 passed all 115 tests. Linux ARM64 exposed a race in the descendant signal proof file: file creation could be observed before its contents were fully written. The bounded wait was changed to require the completed marker contents.
- The `macos-15-arm64` job remained unresponsive and the workflow was canceled after more than four minutes without useful job output.
- Dry-run workflow `35370602943` used `version=0.1.8` and `publish=no`. Its Linux jobs found a Unix-only compile mistake in the new polling predicate before any test executed; this was corrected with a guarded `matches!` expression.
- The `macos-26-arm64` job also remained unresponsive and the workflow was canceled after nearly seven minutes without useful job output. Earlier attempts on `macos-14-arm64` and `macos-15-arm64` had the same infrastructure behavior, although `macos-14-arm64` successfully ran the full native suite in workflow `35358179535`.
- Apple Silicon macOS jobs are temporarily skipped after exhausting the available GitHub-hosted ARM runner image labels. This is an infrastructure availability limit, not a source or test failure; Linux validation continues independently.

## Skipped or deferred procedures

- Live Cloudflare OAuth/API/tunnel testing remains intentionally deferred to manual testing; automated tests exercise pure setup logic and packaging without creating real Cloudflare resources.
- Additional Apple Silicon macOS CI runs are deferred until GitHub-hosted ARM runners become responsive again.

## 2026-09-18 — Linux release validation completed

- Dry-run workflow `35371442706` used `version=0.1.9` and `publish=no`. Both Linux architectures passed all 115 native tests and produced successful musl release builds.
- Both package jobs rejected the binaries because `--version` still reported the Cargo manifest version (`0.1.0`) even though the dispatched app version and setup SHA were embedded correctly. Release version reporting was moved to a compile-time `FW_APP_VERSION` value with `CARGO_PKG_VERSION` as the developer-build fallback.
- Local validation built a Windows GNU executable with synthetic `FW_APP_VERSION=9.8.7` metadata and confirmed that it reported `fw 9.8.7`.
- Dry-run workflow `35371975259` used `version=0.1.10` and `publish=no`. Linux AMD64 and ARM64 each passed all 116 native tests, built successfully as musl binaries, passed setup-SHA, help, release-version, declined-setup URL, archive-layout, permission, symlink, and extracted-executable checks, and uploaded both portable archives.
- The workflow completed successfully, and release publication remained disabled. The temporary Windows and Intel macOS exclusions were then removed; the production workflow again includes all platforms, with Apple Silicon returned to the previously successful `macos-14-arm64` label.

## 2026-09-19 — Full native release validation completed

- Dry-run workflow `35415140511` used `version=0.1.15` and `publish=no` from `feat/mac-linux` at `73023a1`.
- Native Linux AMD64 and ARM64 tests, musl builds, archive verification, and packaging passed. Native macOS Intel and Apple Silicon tests, builds, archive verification, and packaging also passed, as did the Windows tests, portable builds, setup-entrypoint checks, and NSIS packaging.
- The first macOS ARM64 package attempt failed only because GitHub's artifact service timed out all five `ListArtifacts` requests. Its build artifact had already uploaded successfully; rerunning the failed jobs cleared the infrastructure error without a code change.
- The final assembly verified and uploaded all nine release files. `Create GitHub release` was skipped because publishing was disabled.
- The `macos-15` Apple Silicon jobs received runner capacity and completed. No runner-capacity limitation remained in the successful run.

## 2026-09-19 — Universal macOS installer validation completed

- Dry-run workflow `35416602581` used `version=0.1.16` and `publish=no` from `feat/mac-linux` at `3ac7d40`.
- The Intel and Apple Silicon binaries were combined into `fw-macos-setup-v0.1.16.pkg`; `pkgbuild` used package identifier `me.rlzy.fw` and the expanded package passed payload, executable, symlink, script, identifier, version, and runtime checks.
- All native Windows, Linux, and macOS build and package jobs passed. No real failures or runner-capacity limitations occurred.
- Final assembly verified and uploaded all ten release files as `complete-release-0.1.16`. `Create GitHub release` was skipped because publishing was disabled.
