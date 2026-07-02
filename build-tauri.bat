@echo off
REM ============================================================================
REM Hermix Desktop — production build script (Phase L1.4, 2026-06-29)
REM
REM Sequence:
REM   1. cargo build hermix-server --release (Rust sidecar)
REM   2. copy hermix.exe + officecli.exe into src-tauri/binaries/
REM      so Tauri's externalBin pick picks them up.
REM   3. pnpm tauri build (NSIS installer)
REM
REM Environment overrides:
REM   SKIP_BACKEND=1              — skip cargo step (frontend-only build)
REM   SKIP_OFFICECLI=1            — skip copying officecli.exe (some teams
REM                                  stopped using it; see git log e9ad6f5)
REM   BUNDLED_PYTHON=1            — fetch python-build-standalone into
REM                                  <repo>/release/target/release/python/
REM                                  and reference it from tauri.conf.json's
REM                                  bundle.resources. Defaults OFF — most
REM                                  users have Python already; we don't
REM                                  want to inflate the installer by ~30MB
REM                                  without an opt-in.
REM ============================================================================

set "MSVC_ROOT=C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.44.35207"
set "SDK_ROOT=C:\Program Files (x86)\Windows Kits\10"
set "SDK_VER=10.0.26100.0"
set "LIB=%MSVC_ROOT%\lib\x64;%SDK_ROOT%\Lib\%SDK_VER%\um\x64;%SDK_ROOT%\Lib\%SDK_VER%\ucrt\x64"
set "INCLUDE=%MSVC_ROOT%\include;%SDK_ROOT%\Include\%SDK_VER%\ucrt;%SDK_ROOT%\Include\%SDK_VER%\um;%SDK_ROOT%\Include\%SDK_VER%\shared"
set "PATH=%MSVC_ROOT%\bin\Hostx64\x64;%SDK_ROOT%\bin\%SDK_VER%\x64;<USERPROFILE>\.workbuddy\binaries\node\versions\22.22.2;%PATH%"

set "REPO_ROOT=<repo>"
set "EXTERNAL_TARGET=%REPO_ROOT%\..\release\target\release"
set "BINARIES_DIR=%REPO_ROOT%\apps\desktop-tauri\src-tauri\binaries"

REM ---------- 1. Cargo build the sidecar ---------------------------------
if "%SKIP_BACKEND%"=="1" goto SKIP_BACKEND
echo [build-tauri] cargo build hermix-server --release ...
pushd "%REPO_ROOT%\apps\desktop-tauri"
call cargo build -p hermix-server --release
if errorlevel 1 (
  echo [build-tauri] cargo build FAILED
  popd
  exit /b 1
)
popd
:SKIP_BACKEND

REM ---------- 2. Copy hermix.exe + officecli.exe to externalBin slot ----
echo [build-tauri] copying hermix.exe to externalBin slot ...
if not exist "%BINARIES_DIR%" mkdir "%BINARIES_DIR%"
copy /y "%EXTERNAL_TARGET%\hermix.exe" "%BINARIES_DIR%\hermix-x86_64-pc-windows-msvc.exe" >nul
if errorlevel 1 (
  echo [build-tauri] hermix.exe copy FAILED
  exit /b 1
)

if not "%SKIP_OFFICECLI%"=="1" (
  echo [build-tauri] copying officecli.exe ...
  if exist "%EXTERNAL_TARGET%\officecli.exe" (
    copy /y "%EXTERNAL_TARGET%\officecli.exe" "%BINARIES_DIR%\officecli-x86_64-pc-windows-msvc.exe" >nul
  ) else (
    echo [build-tauri] officecli.exe not found at %EXTERNAL_TARGET% - skipping (set SKIP_OFFICECLI=0 to fail-fast)
  )
)

REM ---------- 3. Optional: bundle Python via python-build-standalone ----
if "%BUNDLED_PYTHON%"=="1" (
  echo [build-tauri] fetching python-build-standalone ...
  REM TODO(L1.5): download cpython 3.11.x + extract to %EXTERNAL_TARGET%\python\.
  REM   Layout must match `backends::code_execution::python_search_paths`:
  REM     Windows: %EXTERNAL_TARGET%\python\python.exe
  REM     Unix:    %EXTERNAL_TARGET%\python\bin\python3
  REM   Then add `bundle.resources: ["python/**"]` to tauri.conf.json.
  echo [build-tauri] NOTE: BUNDLED_PYTHON=1 is recorded but the fetcher is still a TODO. Set to 0 to skip.
)

REM ---------- 4. tauri build (NSIS installer) ----------------------------
echo [build-tauri] pnpm tauri build ...
pushd "%REPO_ROOT%\apps\desktop-tauri"
call pnpm tauri build
if errorlevel 1 (
  echo [build-tauri] pnpm tauri build FAILED
  popd
  exit /b 1
)
popd

echo [build-tauri] DONE.
