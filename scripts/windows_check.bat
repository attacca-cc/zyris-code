@echo off
setlocal enabledelayedexpansion
rem ============================================================================
rem  zyris-code — Windows check
rem
rem  Fetches the develop branch, builds it, runs everything that can run without
rem  a person watching, and writes a report next to this file.
rem
rem  Usage:   windows_check.bat [work-dir]
rem           work-dir defaults to %TEMP%\zyris-code-check
rem
rem  What it cannot do: the pty smoke scripts under scripts\ are all built on the
rem  unix `pty` module and do not run here. The parts of this app that only break
rem  on a real terminal are therefore left to the checklist at the end of the
rem  report, which a person fills in.
rem ============================================================================

rem UTF-8, or the width section below prints mojibake and proves nothing.
chcp 65001 >nul 2>&1

set "BRANCH=develop"
set "REPO=https://github.com/attacca-cc/zyris-code.git"
set "WORKDIR=%~1"
if "%WORKDIR%"=="" set "WORKDIR=%TEMP%\zyris-code-check"

rem Timestamp for the report name. WMIC is gone on recent Windows, so fall back.
for /f "usebackq tokens=*" %%t in (`powershell -NoProfile -Command "Get-Date -Format yyyy-MM-dd-HHmm"`) do set "STAMP=%%t"
if "%STAMP%"=="" set "STAMP=report"
set "REPORT=%~dp0windows-check-%STAMP%.md"

echo.
echo   zyris-code Windows check
echo   branch    %BRANCH%
echo   work dir  %WORKDIR%
echo   report    %REPORT%
echo.

> "%REPORT%" echo # zyris-code — Windows check
>> "%REPORT%" echo.
>> "%REPORT%" echo Run at %DATE% %TIME% on branch `%BRANCH%`.
>> "%REPORT%" echo.

rem ---------------------------------------------------------------- machine --
call :section "Machine"
call :run "ver" ver
call :run "systeminfo (OS name and version)" powershell -NoProfile -Command "(Get-CimInstance Win32_OperatingSystem) | Select-Object Caption,Version,OSArchitecture | Format-List"

rem ------------------------------------------------------------------ tools --
call :section "Toolchain"
where git >nul 2>&1
if errorlevel 1 (
  call :fail "git is not on PATH. Install it from https://git-scm.com/download/win and run this again."
  goto :finish
)
where cargo >nul 2>&1
if errorlevel 1 (
  call :fail "cargo is not on PATH. Install Rust from https://rustup.rs and run this again."
  goto :finish
)
call :run "git --version" git --version
call :run "rustc --version" rustc --version
call :run "cargo --version" cargo --version

rem ------------------------------------------------------------------ fetch --
call :section "Source"
if exist "%WORKDIR%\.git" (
  echo   updating existing checkout...
  pushd "%WORKDIR%"
  call :run "git fetch" git fetch origin %BRANCH% --depth 50
  call :run "git checkout" git checkout -B %BRANCH% origin/%BRANCH%
) else (
  echo   cloning...
  if not exist "%WORKDIR%" mkdir "%WORKDIR%"
  call :run "git clone" git clone --branch %BRANCH% --depth 50 "%REPO%" "%WORKDIR%"
  pushd "%WORKDIR%"
)
call :run "git log -1" git log -1 --format="%%H %%an %%ad %%s" --date=short

rem ------------------------------------------------------------------ build --
call :section "Build"
call :run "cargo build --workspace" cargo build --workspace

call :section "Tests"
call :run "cargo test --workspace" cargo test --workspace

call :section "Clippy"
call :run "cargo clippy --workspace --all-targets" cargo clippy --workspace --all-targets

rem ------------------------------------------------------- terminal capability
call :section "What this terminal looks like from inside the app"
call :run "cargo run --example term_report" cargo run --quiet --example term_report

popd

rem -------------------------------------------------------------- eyes-only --
echo.
echo   ------------------------------------------------------------------
echo   The width check below has to be looked at, not parsed. Every ']'
echo   should sit in the SAME column. One that is further right means that
echo   character is drawn two columns wide here, which shifts every cell
echo   after it and is the main reason this app breaks on some terminals.
echo   ------------------------------------------------------------------
echo.
pushd "%WORKDIR%"
cargo run --quiet --example term_report 2>nul | findstr /C:"]"
popd
echo.

call :section "Checklist — please fill in"
>> "%REPORT%" echo These need a person and a real terminal. Replace [ ] with [x] or [FAIL],
>> "%REPORT%" echo and add a line under anything that failed.
>> "%REPORT%" echo.
>> "%REPORT%" echo Terminal used: ^<Windows Terminal / cmd.exe / PowerShell window / other^>
>> "%REPORT%" echo.
>> "%REPORT%" echo - [ ] The app starts and draws a screen (`cargo run -p zyris-code`)
>> "%REPORT%" echo - [ ] Typing shows in the input box, Backspace deletes
>> "%REPORT%" echo - [ ] Ctrl+U clears back to the start, Ctrl+W deletes a word, Ctrl+Y puts it back
>> "%REPORT%" echo - [ ] Shift+Enter makes a newline (if not, Alt+Enter must)
>> "%REPORT%" echo - [ ] Arrow keys and Home/End move the cursor
>> "%REPORT%" echo - [ ] Shift+Tab cycles the mode shown at the bottom left
>> "%REPORT%" echo - [ ] `/` opens the command list and typing narrows it
>> "%REPORT%" echo - [ ] Mouse wheel scrolls the conversation
>> "%REPORT%" echo - [ ] Dragging over text highlights it
>> "%REPORT%" echo - [ ] After a drag, Ctrl+V into Notepad pastes that text
>> "%REPORT%" echo - [ ] Dragging over the enrolment code window highlights it too
>> "%REPORT%" echo - [ ] Ctrl+click on a link opens a browser
>> "%REPORT%" echo - [ ] A link is underlined only while the pointer is over it
>> "%REPORT%" echo - [ ] Resizing the window redraws cleanly, no leftover characters
>> "%REPORT%" echo - [ ] Ctrl+L redraws the screen
>> "%REPORT%" echo - [ ] Ctrl+C once stops a running turn, twice quits
>> "%REPORT%" echo - [ ] After quitting, the shell is normal again (colours, cursor, long lines wrap)
>> "%REPORT%" echo - [ ] No stray escape characters anywhere on screen
>> "%REPORT%" echo.
>> "%REPORT%" echo If something failed, run again with these and say whether it changed:
>> "%REPORT%" echo.
>> "%REPORT%" echo ```
>> "%REPORT%" echo set ZYRIS_CODE_MOUSE=0        ^&rem hand selection back to the terminal
>> "%REPORT%" echo set ZYRIS_CODE_HYPERLINKS=0   ^&rem stop sending OSC 8
>> "%REPORT%" echo set ZYRIS_CODE_OSC52=0        ^&rem stop sending clipboard writes
>> "%REPORT%" echo set ZYRIS_CODE_HEAL_MS=300    ^&rem redraw more often, for leftover characters
>> "%REPORT%" echo ```

:finish
echo.
echo   Done. Report written to:
echo   %REPORT%
echo.
echo   Please fill in the checklist at the end of it and send the file back.
echo.
pause
endlocal
exit /b 0

rem ---------------------------------------------------------------- helpers --

:section
>> "%REPORT%" echo.
>> "%REPORT%" echo ## %~1
>> "%REPORT%" echo.
echo   == %~1
exit /b 0

:run
rem %1 is the label, the rest is the command.
set "LABEL=%~1"
shift
set "CMD=%1"
:build_cmd
shift
if "%1"=="" goto :run_it
set "CMD=!CMD! %1"
goto :build_cmd
:run_it
echo   - %LABEL%
>> "%REPORT%" echo ### %LABEL%
>> "%REPORT%" echo.
>> "%REPORT%" echo ```
!CMD! >> "%REPORT%" 2>&1
set "RC=!errorlevel!"
>> "%REPORT%" echo ```
>> "%REPORT%" echo.
if not "!RC!"=="0" (
  echo       FAILED with exit code !RC!
  >> "%REPORT%" echo **FAILED — exit code !RC!**
  >> "%REPORT%" echo.
) else (
  >> "%REPORT%" echo OK.
  >> "%REPORT%" echo.
)
exit /b 0

:fail
echo   !! %~1
>> "%REPORT%" echo **%~1**
>> "%REPORT%" echo.
exit /b 0
