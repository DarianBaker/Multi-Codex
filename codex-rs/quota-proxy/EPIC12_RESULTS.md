# `multi-codex-gui-installer` - real-run verification results

Real, observed output from actually running the built `multi-codex-installer.exe`
(release build) - never against the real `PATH` or the real
`%LOCALAPPDATA%\multi-codex`. Every run below used
`MULTI_CODEX_WIZARD_TEST_ENV_VAR` to point the installer at a throwaway user
environment variable instead of `PATH`, and `MULTI_CODEX_WIZARD_INSTALL_DIR`
to point it at a scratch directory instead of the real per-user install
location. Every scratch value was deleted afterward.

Built with `cargo build --release -p codex-quota-proxy -p codex-cli --bin
codex-quota-proxy --bin multi-codex --bin multi-codex-wizard --bin codex`,
then `cargo build --release -p multi-codex-gui-installer` (which
`include_bytes!`s the four sibling `.exe`s from `target/release/`, so they
must exist first).

Because neither computer-use nor File-Explorer-mediated interaction were
available in this sandbox, GUI verification here is entirely non-visual: the
installer is launched, its child window handles are enumerated via
`EnumChildWindows`, the Install button is clicked via `SendMessage(...,
BM_CLICK, ...)`, and the resulting status-label text and on-disk state are
read back - all through raw Win32 calls from PowerShell. This proves the
install logic actually runs and actually writes the right files; it does not
prove the window looks correct on screen.

## Design change from the original GUI installer

Original design (see `git log` around `c24db774d`) bundled an in-GUI account
label/login section that shelled out to `multi-codex.exe login` in a new
console window. Per explicit feedback: the GUI's only job is install + PATH
setup; everything else - logging in, adding accounts - happens via
`multi-codex login <label>` / `multi-codex setup` on the CLI after install.
The account section (label input, main-account checkbox, Log In button, the
`on_login` handler, the `CREATE_NEW_CONSOLE` process flag) was removed
entirely; the window shrank from 440×330 to 440×220 and the post-install
message now just points at the CLI commands.

**Verified:** ran the installer against a scratch install dir + scratch env
var, clicked Install via `BM_CLICK`, read back the status label:
```
Installed to <scratch>\install. Open a new terminal and run `multi-codex login <label>` or `multi-codex setup` to add accounts.
```
No account/login controls exist in the child-window enumeration.

## `codex-code-mode-host.exe` bundling

The installer now also embeds and writes `codex-code-mode-host.exe` - the
V8-embedding "Code Mode" helper binary - alongside the other three. Getting
this binary to exist at all on Windows was most of this epic's work; see
below.

**Verified, real binaries, scratch install dir + scratch env var:**
```
multi-codex.exe            18,055,680 bytes
codex-quota-proxy.exe      17,917,952 bytes
codex.exe                 295,541,248 bytes
codex-code-mode-host.exe   53,963,776 bytes
```
All four present in the scratch install directory after clicking Install;
the scratch env var was confirmed (via a separate `powershell.exe` call, not
the installer's own code) to hold the scratch install path afterward.

## Building `codex-code-mode-host.exe`: the V8-sandbox-on-Windows problem

`code-mode-runtime` depends on the `v8` crate with `features =
["v8_enable_sandbox"]` (pointer-compression + heap sandboxing - the
mechanism that keeps a memory-corruption bug in AI-generated JS contained
within V8's own heap, not the whole host process). Confirmed via the GitHub
Releases API that **no Windows prebuilt has ever been published for this
variant** - neither for the pinned `v8` version (150.4.0) nor latest
(152.1.0) - only plain `release` and `simdutf_release`. rusty_v8's own docs
call the sandboxed build "experimental," "not well tested," and say it
"does not undergo any sort of CI related testing or prebuilt archives" on
any platform. This is a permanent upstream gap, not something waiting to be
published.

Given the choice already made (build from source once, at release-build
time, rather than ship an unsandboxed V8 to every end user), the rest of
this epic was making `V8_FROM_SOURCE=1 cargo build --release -p
codex-code-mode-host` actually succeed on Windows. It did, eventually.
Every blocker actually hit, and the fix, in order:

1. **`cargo` autobins picking up a stray test file, missing vendor crates,
   `icudtl.dat`** - the crates.io `v8` package is not a full `gclient sync`
   checkout; it's missing the entire `third_party/rust/chromium_crates_io/
   vendor/` tree and one ICU locale-data variant. Fetched the ~64 missing
   crates individually from crates.io (exact versions read from each
   target's `BUILD.gn`) into the right `vendor/<name>-<gn_epoch>/` paths;
   copied an existing larger ICU variant into the expected path.

2. **`GetFullPathNameA(...): The filename or extension is too long.`** - a
   genuine Windows `MAX_PATH` (260-char) failure, hit only for specific
   deeply-nested `third_party/` header paths once combined with this
   environment's already-long absolute checkout path (`C:\Users\
   daria.THE_FLASH\scoop\...\v8-150.4.0\...`). This matches Chromium's own
   documented recommendation to keep Windows checkouts at a short path like
   `C:\src`. Fixed by mirror-copying the whole ~2.65 GB source tree to
   `C:\v8src` (robocopy, which handles long paths natively) and pointing
   Cargo at it via `--config patch.crates-io.v8.path="C:/v8src"` - a
   per-invocation flag, nothing committed, since the fix is specific to this
   machine's checkout depth, not a real code change.

3. **`Unable to find libclang`** - bindgen (used to generate the V8 C++ →
   Rust FFI bindings) needs a real `libclang.dll`; Chromium's own downloaded
   `clang-cl.exe` is compiler-only and does not ship one. Installed a full
   LLVM via `scoop install llvm` and pointed `LIBCLANG_PATH` at it.

4. **`ninja: error: ... icudtl.dat ... missing`, then `CMAKE_AR-NOTFOUND`,
   then `ml64`/`rc.exe` not found** - all instances of the same root cause:
   this environment's raw shell has none of the MSVC toolchain's `INCLUDE` /
   `LIB` / `PATH` entries that a normal "Developer Command Prompt" gets from
   `vcvars64.bat`. Captured them for real (`cmd /c "vcvars64.bat && set"`)
   and exported them into every subsequent build invocation. (Hit and fixed
   a real bug in my own first attempt at this: `export $(cat file)` word-
   splits on the spaces in `C:\Program Files\...`, silently truncating
   `INCLUDE` to `C:\Program`. Fixed by building the export statements with
   proper quoting via PowerShell instead of naive shell substitution.)

5. **Bindgen failed with `reference to unresolved using declaration` /
   `using declaration annotated with 'using_if_exists' here` in libc++'s
   `__memory_resource/memory_resource.h` → `__cstddef/max_align_t.h`, and
   separately `'new.h' file not found`** - the real, final blocker, and the
   one that took the most wrong turns to actually diagnose:

   - **First hypothesis (wrong): clang version mismatch.** Chromium bundles
     its own bleeding-edge dev build of clang (`23.0.0git`, an unreleased
     pinned commit `20b6ec66967ac2a8f932863c1abf251e5b17a843`), while
     bindgen was using scoop's stable LLVM 22.1.8 to parse the same headers.
     Reasonable theory, explicitly signed off on by you as worth the cost of
     testing. Built a matching `libclang.dll` from that exact upstream
     commit from source (downloaded the ~250 MB source archive at that SHA,
     configured a minimal CMake+Ninja build restricted to the `clang`
     project / X86 target / `libclang` shared-lib target only, using
     Chromium's own `clang-cl.exe` as the host compiler - hit and fixed
     `rc.exe`, `ml64.exe`, and `lib.exe` all missing from `PATH` along the
     way, then a full ~2830-step compile). **Result: identical error, byte
     for byte.** This conclusively disproved the version-mismatch theory -
     worth building and testing rather than assuming, since it ruled out
     what looked like the most likely cause and pointed the actual
     investigation somewhere real.
   - **Second hypothesis (partially right, insufficient alone): missing
     `--target=x86_64-pc-windows-msvc` / MSVC-compatibility flags.**
     `build.rs` has explicit branches adding target triples and SDK/sysroot
     flags for macOS, Linux, and iOS - genuinely no branch at all for
     Windows. Added one (`--target=x86_64-pc-windows-msvc
     -fms-compatibility -fms-extensions -fdelayed-template-parsing`).
     Flags were confirmed applied (a new deprecation warning appeared) but
     the identical error persisted.
   - **Actual root cause, found by reading the headers directly instead of
     guessing more flags:** `<cstddef>`'s `max_align_t.h` does `#include
     <stddef.h>` immediately before `using ::max_align_t
     _LIBCPP_USING_IF_EXISTS;`. Confirmed by `grep` that the Windows SDK's
     own `ucrt/stddef.h` (10.0.26100.0) **does not define `max_align_t` at
     all** - while clang's own resource-dir `stddef.h` does, via
     `__stddef_max_align_t.h`. Whichever `<stddef.h>` a given `#include`
     resolves to first wins; under bindgen's raw libclang invocation it was
     resolving to UCRT's (no `max_align_t`), so `using_if_exists` correctly
     - and unhelpfully - produced nothing, and the later, unconditional
     reference to `std::max_align_t` in `memory_resource.h` /
     `polymorphic_allocator.h` then hard-failed. Real `clang-cl` compiling
     V8's own `.cc` files never hits this, most likely because none of
     V8's own translation units happen to pull in `<memory_resource>` -
     bindgen's `binding.hpp` (the FFI surface) does.

     **Fix:** added `-isystem<clang resource-dir>/include` to bindgen's
     `clang_args` on Windows, forcing `<stddef.h>` to resolve to clang's own
     version (which defines `max_align_t`) ahead of UCRT's. Three lines,
     once the actual cause was known.

**Verified:** `V8_FROM_SOURCE=1 cargo build --release -p
codex-code-mode-host --config patch.crates-io.v8.path="C:/v8src"` completed
clean - `Compiling v8 v150.4.0`, `Compiling codex-code-mode-runtime`,
`Compiling codex-code-mode-host`, `Finished release profile [...] in
57.26s` (C++ was already cached from the earlier attempts; only bindgen +
the Rust crates needed to re-run). The resulting `codex-code-mode-host.exe`
(54 MB) runs and correctly rejects an unrecognized `--version` flag with its
real `clap`-generated usage text, rather than crashing.

## Interruption during verification: Windows Update, not us

Partway through rebuilding the bundled binaries, a background build lost its
completion signal and several MCP tool connections dropped and reconnected.
Checked `Get-CimInstance Win32_OperatingSystem`'s `LastBootUpTime` and the
System event log rather than guessing: two restarts were initiated by
`TrustedInstaller.exe` and `MoUsoCoreWorker.exe` (both OS-servicing
components) on behalf of `NT AUTHORITY\SYSTEM`, reason "Operating System:
Upgrade (Planned)" / "Service pack (Planned)" - a scheduled Windows Update
reboot, not anything this session issued. All four built binaries and the
`C:\v8src` / `C:\llvm-build` build trees survived on disk untouched (only
the in-flight `cargo` process was killed); the installer rebuild was simply
re-launched from where it left off.

## What still needs you

- Whether the installer window actually **looks** right - title, label
  wrapping, button layout - needs a real screen, same limitation noted in
  the original GUI-installer verification.
- Whether Code Mode itself actually works end-to-end (spawns
  `codex-code-mode-host.exe`, executes real JS/TS, the sandbox actually
  contains a deliberately-bad script) hasn't been exercised here - this epic
  only proves the binary *builds* and the installer *bundles* it correctly.
