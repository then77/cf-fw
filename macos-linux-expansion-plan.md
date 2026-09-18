# macOS and Linux Expansion Plan

## Goal

Extend FW from its stable Windows implementation to macOS and Linux while preserving:

- One daemon per Windows user and installation directory.
- One daemon per Unix runtime environment and installation directory.
- Separate FW installations using separate Cloudflare tunnels and domains.
- The current sibling `cf` installation model unless explicitly redesigned.
- Transactional setup with integrity verification and rollback.
- A single GitHub release that can contain Windows, macOS, and Linux assets.

Windows behavior must remain unchanged while the shared platform interfaces are introduced.

## Execution guardrails

- All macOS/Linux work must remain on `feat/mac-linux`; do not modify another branch unless the user explicitly requests it.
- Add tests with each platform feature and pursue complete meaningful coverage without weakening assertions, bypassing security checks, or introducing test-only production behavior.
- If a required procedure is unsafe, excessively brittle, or cannot be validated honestly, record it in `macos-linux-expansion-logs.md`, skip that procedure, and report it to the user.
- Use local/source checks during implementation, but dispatch the release workflow only for the pre-build test-suite run immediately before a build attempt and for the build itself.
- Every experimental workflow dispatch must set `publish=no`; prerelease/release publishing requires a separate explicit user decision.
- Remote build iteration uses authenticated `gh` commands, with failing logs inspected before applying the smallest root-cause fix.

## Initial release targets

The target matrix follows architectures for which Cloudflare publishes compatible `cloudflared` artifacts.

### Required first targets

| Platform | Rust target | Cloudflared artifact |
| --- | --- | --- |
| macOS Intel | `x86_64-apple-darwin` | `cloudflared-darwin-amd64.tgz` |
| macOS Apple Silicon | `aarch64-apple-darwin` | `cloudflared-darwin-arm64.tgz` |
| Linux AMD64 | `x86_64-unknown-linux-musl` | `cloudflared-linux-amd64` |
| Linux ARM64 | `aarch64-unknown-linux-musl` | `cloudflared-linux-arm64` |

### Follow-up Linux targets

| Platform | Rust target | Cloudflared artifact |
| --- | --- | --- |
| Linux 386 | `i686-unknown-linux-musl` | `cloudflared-linux-386` |
| Linux ARM hard-float | `armv7-unknown-linux-musleabihf` | `cloudflared-linux-armhf` |

The implementation should understand all planned architecture names, but AMD64 and ARM64 should be stabilized before enabling the secondary release artifacts.

Portable Linux artifacts should prefer musl to avoid binding downloads to a particular glibc baseline.

## Runtime identity

`app_hash` means a stable hash of the directory containing the physical FW executable. It does not include the executable filename or executable contents.

Consequences:

- Multiple terminals using one installation share one daemon.
- Renaming or replacing FW in the same directory preserves daemon identity.
- Copies in separate directories use separate daemons and may use different Cloudflare tunnels and domains.
- Unix path hashing uses raw path bytes and remains case-sensitive.
- Windows path hashing retains its current normalized, case-insensitive behavior.

## IPC names

### Windows

Preserve the existing names:

```text
\\.\pipe\fw-{sid_hash}-{app_hash}
Local\fw-daemon-{sid_hash}-{app_hash}
Local\fw-start-{sid_hash}-{app_hash}
```

### Linux

Use the user-specific XDG runtime directory:

```text
$XDG_RUNTIME_DIR/fw-{app_hash}.sock
$XDG_RUNTIME_DIR/fw-daemon-{app_hash}.lock
$XDG_RUNTIME_DIR/fw-start-{app_hash}.lock
```

`XDG_RUNTIME_DIR` must be absolute, owned by the current user, and not group/world writable. An unavailable or unsafe runtime directory should produce an actionable error rather than silently falling back to shared `/tmp`.

### macOS

Use the normally user-specific temporary directory:

```text
$TMPDIR/fw-{app_hash}.sock
$TMPDIR/fw-daemon-{app_hash}.lock
$TMPDIR/fw-start-{app_hash}.lock
```

Validate that the selected directory is safe before creating locks or sockets.

## Phase 1: Common platform and IPC facades

Refactor callers so `src/main.rs`, `src/daemon/mod.rs`, and other shared modules do not import Windows named-pipe or mutex types directly.

### `src/platform/mod.rs`

Expose target-neutral operations for:

- Runtime names and paths.
- Daemon ownership guard.
- Startup serialization guard.
- Daemon process spawning.
- Child-process lifecycle operations.
- Atomic replacement where platform behavior differs.

Select implementations with `#[cfg(windows)]` and `#[cfg(unix)]`.

### `src/ipc/mod.rs`

Expose target-neutral transport types and functions:

- `Stream`
- `Listener`
- `connect`

Keep framing and protocol modules transport-independent.

### Windows regression safety

Adapt `src/platform/windows.rs` and `src/ipc/windows.rs` to the common facades without changing existing pipe, mutex, process, job-object, or path behavior.

## Phase 2: Unix platform implementation

Add `src/platform/unix.rs`.

Responsibilities:

1. Resolve and validate Linux/macOS runtime directories.
2. Hash Unix installation paths without lossy UTF-8 conversion.
3. Acquire kernel-backed daemon and startup file locks.
4. Retain lock file descriptors for each guard's lifetime.
5. Spawn the daemon in a new session/process group.
6. Send signals to owned child process groups.
7. Perform atomic filesystem replacement safely.

The Unix daemon must not remain in the foreground terminal's process group. Otherwise Ctrl+C in a foreground FW client could also terminate the daemon and `cloudflared`.

## Phase 3: Unix-domain socket transport

Add `src/ipc/unix.rs` using:

```rust
tokio::net::UnixListener
tokio::net::UnixStream
```

Required behavior:

- Bind only after daemon ownership is established.
- Set socket permissions to `0600`.
- Reject pre-existing directories, symlinks, and unexpected path types.
- Remove sockets during orderly daemon shutdown.
- Recover stale sockets only while startup is serialized and no live daemon owns the daemon lock.
- Keep path lengths within Linux and macOS Unix-socket limits.
- Never allow one client to unlink an active daemon's socket.

Tests:

- Client/server round trip.
- Concurrent clients.
- Socket cleanup.
- Stale socket recovery.
- Unsafe path rejection.
- Socket permissions.
- Same-directory executable rename stability.
- Separate-directory daemon isolation.

## Phase 4: Unix daemon and process lifecycle

Add Unix signal handling to the daemon:

- Internal `fw kill` follows the existing cancellation path.
- Idle shutdown remains supported.
- `SIGTERM` requests orderly shutdown.
- `SIGINT` requests orderly shutdown when daemon mode is run directly.
- Listener cleanup occurs after the accept loop exits.

Update `src/cloudflared.rs` so Unix process handling:

1. Spawns `cloudflared` in an owned process group.
2. Sends `SIGTERM` during normal shutdown.
3. Waits for a bounded grace period.
4. Escalates to `SIGKILL` when needed.
5. Reaps the process.
6. Prevents surviving descendants.

Windows keeps its current kill-on-close Job Object behavior.

## Phase 5: Cross-platform installation paths

Make the cloudflared executable name target-specific:

```rust
#[cfg(windows)]
pub const CLOUDFLARED_FILENAME: &str = "cloudflared.exe";

#[cfg(unix)]
pub const CLOUDFLARED_FILENAME: &str = "cloudflared";
```

The first Unix implementation retains the existing layout:

```text
installation/
├── fw or fw.exe
└── cf/
    ├── cloudflared or cloudflared.exe
    └── config.yml
```

This requires a user-writable installation directory. Unix setup must not invoke `sudo` internally.

A future native package layout may separate immutable binaries from mutable data:

- Linux config under XDG directories.
- macOS data under `~/Library/Application Support/FW`.
- Executable in a conventional PATH or application location.

That redesign is intentionally outside the initial portable release unless required for a real signed `.app`, `.deb`, or `.rpm`.

## Phase 6: Cross-platform setup bootstrapper

Refactor `src/setup.rs` into shared download/integrity logic and target-specific execution.

### Setup assets

| Platform | Asset |
| --- | --- |
| Windows | `fw-setup.ps1` |
| macOS/Linux | `fw-setup.sh` |

Each platform build embeds:

- `FW_APP_VERSION`
- The SHA-256 of the setup asset used by that target.

The existing `FW_SETUP_SCRIPT_SHA` variable can remain because each compiled target needs exactly one setup script hash.

### Windows execution

Preserve the current locked PowerShell script verification and `pwsh.exe`/`powershell.exe` launch behavior.

### Unix execution

Download `fw-setup.sh`, verify its size and SHA-256, and pass the already-verified bytes to:

```text
/bin/sh -s -- --fw-path <absolute-fw-path>
```

Streaming verified bytes to standard input avoids reopening a pathname after verification and reduces replacement races.

## Phase 7: `fw-setup.sh`

Create a POSIX-compatible `setup/fw-setup.sh` that faithfully migrates the PowerShell setup transaction.

### Required setup phases

1. Preflight all required tools before remote mutation.
2. Validate the FW path and writable installation directory.
3. Create and validate the sibling `cf` directory.
4. Run Cloudflare OAuth with PKCE and a loopback callback.
5. Select the Cloudflare account and zone.
6. Validate the wildcard hostname.
7. Detect existing DNS and certificate resources.
8. Offer the existing Worker certificate workaround where needed.
9. Select and download the correct `cloudflared` artifact.
10. Verify the pinned cloudflared SHA-256.
11. Create the locally managed tunnel and credentials.
12. Create the wildcard DNS record.
13. Generate and validate `config.yml`.
14. Run local, tunnel, certificate, DNS, and HTTPS checks.
15. Roll back local and Cloudflare resources after failure or interruption.
16. Revoke OAuth tokens on success or failure.

### Shell and dependencies

Target strict POSIX `sh`, not Bash-specific syntax.

Expected tools:

- `/bin/sh`
- `curl`
- `jq`
- `python3`
- `openssl`
- `tar`
- `mktemp`
- standard filesystem and process tools

Use Python for the bounded loopback OAuth callback server and local test HTTP server. Do not use ad hoc `nc` parsing for OAuth HTTP requests.

The script should report all missing dependencies together before OAuth begins.

### Browser opening

```text
macOS: open <authorization-url>
Linux: xdg-open <authorization-url>
```

If no browser opener is available, print the URL and continue waiting.

### Unix permissions

```text
new cf directory:      0700
credentials file:      0600
temporary secret data: 0600
cloudflared executable: 0755
Unix socket:           0600
```

Use `umask 077`, secure `mktemp`, same-directory staging, atomic rename, bounded network operations, and traps for `EXIT`, `INT`, `TERM`, and `HUP`.

Do not place OAuth tokens, authorization codes, PKCE verifiers, or tunnel secrets in command-line arguments or logs.

### Cloudflared integrity

Before pinning macOS archive hashes, independently download and hash the exact Darwin `.tgz` bytes. The current Cloudflare release API digest and checksum text have shown inconsistent Darwin values.

Validate archive member paths before extraction and install only the expected `cloudflared` executable.

## Phase 8: Distribution formats

### Windows

Keep current outputs unchanged:

```text
fw-windows-setup-v<version>.exe
fw-windows-portable-i386-v<version>.exe
fw-windows-portable-amd64-v<version>.exe
fw-setup.ps1
```

### macOS

Initial portable outputs:

```text
fw-macos-portable-amd64-v<version>.tar.gz
fw-macos-portable-arm64-v<version>.tar.gz
```

The archive preserves executable permissions and may be extracted or dragged as a folder into a user-selected writable location.

Do not initially build a conventional `.app` bundle. FW is a terminal CLI, and placing mutable sibling `cf` data inside `FW.app/Contents/MacOS` would mutate the bundle and invalidate code signing. A signed/notarized app or package requires a separate native data-location design.

### Linux

Initial outputs:

```text
fw-linux-portable-amd64-v<version>.tar.gz
fw-linux-portable-arm64-v<version>.tar.gz
```

Follow-up outputs:

```text
fw-linux-portable-386-v<version>.tar.gz
fw-linux-portable-armhf-v<version>.tar.gz
```

Do not initially publish `.deb` or `.rpm` packages while runtime configuration remains writable beside the executable.

Publish one shared Unix setup script:

```text
fw-setup.sh
```

## Phase 9: Release workflow

Extend `.github/workflows/release.yml` with platform-specific jobs:

```text
build-windows
package-windows
build-macos
package-macos
build-linux
package-linux
release
```

The final `release` job remains generic and aggregates every platform artifact.

### Release publishing mode

The manual workflow input is:

```text
Publish release:
- no
- pre-release
- release
```

Behavior:

- `no`: build, package, upload workflow artifacts, and verify release contents, but do not create a tag or GitHub release.
- `pre-release`: create a GitHub prerelease.
- `release`: create a normal GitHub release.

Use `no` during the macOS/Linux experimentation loop.

### macOS jobs

Build and test:

```text
x86_64-apple-darwin
aarch64-apple-darwin
```

Use compatible Intel and Apple Silicon runners where available. Test each native binary on the matching architecture rather than only cross-linking it.

### Linux jobs

Use pinned cross-compilation tooling for musl targets. Start with AMD64 and ARM64, then enable 386 and ARMv7 after the common workflow is stable.

### Release contents

Publish:

- Windows installer and portable executables.
- macOS portable archives.
- Linux portable archives.
- `fw-setup.ps1`.
- `fw-setup.sh`.

Replace the current exactly-four-files assertion with an explicit complete expected-file list.

### Permissions

Keep least-privilege job permissions:

```yaml
build-*:
    permissions:
        contents: read

package-*:
    permissions:
        contents: read

release:
    permissions:
        contents: write
```

The release action can use `${{ github.token }}`; no custom PAT is required for publishing to the same repository unless repository or organization policy blocks it.

## Phase 10: Validation

### Local/source validation

Run formatting and target-appropriate tests:

```text
cargo fmt --all
cargo test --target x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
cargo test --target x86_64-unknown-linux-gnu
sh -n setup/fw-setup.sh
shellcheck -s sh setup/fw-setup.sh
```

### IPC integration tests

Verify:

- Competing clients start exactly one daemon.
- One installation shares one daemon.
- Separate installation directories use separate daemons and tunnels.
- Renaming the executable in one directory preserves identity.
- Stale Unix sockets recover safely.
- Runtime paths reject unsafe ownership, modes, and symlinks.
- Socket and credentials permissions are correct.
- `SIGTERM`, `fw kill`, and idle shutdown cleanly stop `cloudflared`.
- No child or descendant processes survive shutdown.

### Packaging tests

For every artifact:

- Extract it.
- Verify expected filenames and executable modes.
- Run `fw --help` on a compatible runner.
- Verify embedded version and setup metadata.
- Confirm setup selects the matching cloudflared artifact.

### GitHub Actions experimentation loop

For each implementation slice:

1. Format and run whatever local checks are available.
2. Commit only the intended files.
3. Push the feature branch.
4. Dispatch `release.yml` with a unique semantic version and `publish=no`.
5. Monitor all jobs to completion.
6. Read failing job logs.
7. Apply the smallest root-cause fix.
8. Repeat until all platform jobs pass.
9. Use `pre-release` for the first downloadable external test.
10. Use `release` only after installation and tunnel smoke tests pass.

## Implementation order

1. Introduce platform and IPC facades while preserving Windows behavior.
2. Implement Unix runtime paths, locks, and Unix-domain sockets.
3. Implement Unix daemon detachment and signal handling.
4. Implement Unix cloudflared process-group shutdown.
5. Make cloudflared filenames and setup bootstrap target-aware.
6. Port the setup transaction to `fw-setup.sh`.
7. Add macOS AMD64 and ARM64 build/package jobs.
8. Add Linux AMD64 and ARM64 build/package jobs.
9. Stabilize the no-publish GitHub Actions loop.
10. Add Linux 386 and ARMv7 if their cross-build and runtime tests are reliable.
11. Publish a prerelease for external macOS/Linux testing.
12. Add signing, notarization, or native package layouts only after portable releases are stable.

## Main risks

1. Unsafe stale Unix-socket removal can disrupt an active daemon.
2. A daemon left in the terminal process group can be killed by a client Ctrl+C.
3. Killing only the immediate cloudflared process can leave descendants alive.
4. Unix socket path limits are shorter than normal filesystem path limits.
5. Mutable sibling configuration conflicts with signed app bundles and system package locations.
6. Reimplementing the transactional PowerShell setup in shell can introduce rollback or quoting errors.
7. macOS setup dependencies such as `jq` and `python3` may not exist on a clean machine.
8. Unsigned macOS downloads may be blocked or warned about by Gatekeeper.
9. Darwin cloudflared archive checksums must be independently verified before pinning.
10. Secondary Linux architectures may compile successfully but lack practical native CI execution coverage.
