@echo off
rem ===========================================================================
rem  zyris-code - Windows check
rem
rem  Fetches the develop branch, builds it, runs everything that can run without
rem  a person watching, and writes a report next to this file.
rem
rem  Usage:   windows_check.bat [work-dir]
rem           work-dir defaults to %TEMP%\zyris-code-check
rem
rem  The pseudo-terminal tests DO run here: tests\pty.rs drives the app through
rem  a ConPTY, so the cargo test below covers drawing, keystrokes and shutdown
rem  on a real console. The older scripts\*.py smokes remain unix-only.
rem
rem  What is left over needs eyes - colours, glyph widths, whether a drag looks
rem  right - and that is the checklist at the end of the report.
rem
rem  THIS FILE MUST KEEP CRLF LINE ENDINGS AND STAY PURE ASCII.
rem  cmd.exe seeks through a batch file by byte offset, so with LF endings the
rem  position drifts by one per line and it starts running from the middle of
rem  lines - "rem" becomes "m", and every line after that loses more. That is
rem  what happened the first time this was handed over. .gitattributes pins it.
rem ===========================================================================

rem UTF-8, so the width section below prints glyphs rather than mojibake.
chcp 65001 >nul 2>&1

set "BRANCH=develop"
set "REPO=https://github.com/attacca-cc/zyris-code.git"
set "WORKDIR=%~1"
if "%WORKDIR%"=="" set "WORKDIR=%TEMP%\zyris-code-check"

rem A fixed name on purpose. %DATE% is formatted by locale, so slicing it for a filename
rem produces different rubbish on every Windows this might run on - and a report that failed
rem to be written is worse than one that overwrote the last.
set "REPORT=%~dp0windows-check.md"

echo.
echo   zyris-code Windows check
echo     branch    %BRANCH%
echo     work dir  %WORKDIR%
echo     report    %REPORT%
echo.

> "%REPORT%" echo # zyris-code - Windows check
>> "%REPORT%" echo.
>> "%REPORT%" echo Run at %DATE% %TIME% on branch %BRANCH%.

rem --------------------------------------------------------------- machine --
echo   == machine
>> "%REPORT%" echo.
>> "%REPORT%" echo ## Machine
>> "%REPORT%" echo.
>> "%REPORT%" echo ```
ver >> "%REPORT%" 2>&1
powershell -NoProfile -Command "(Get-CimInstance Win32_OperatingSystem) | Select-Object Caption,Version,OSArchitecture | Format-List" >> "%REPORT%" 2>&1
>> "%REPORT%" echo ```

rem ----------------------------------------------------------------- tools --
echo   == toolchain
>> "%REPORT%" echo.
>> "%REPORT%" echo ## Toolchain
>> "%REPORT%" echo.
>> "%REPORT%" echo ```
where git >nul 2>&1
if errorlevel 1 (
  echo   !! git is not on PATH. Install from https://git-scm.com/download/win
  >> "%REPORT%" echo git is NOT on PATH - install from https://git-scm.com/download/win
  >> "%REPORT%" echo ```
  goto :done
)
where cargo >nul 2>&1
if errorlevel 1 (
  echo   !! cargo is not on PATH. Install Rust from https://rustup.rs
  >> "%REPORT%" echo cargo is NOT on PATH - install Rust from https://rustup.rs
  >> "%REPORT%" echo ```
  goto :done
)
git --version >> "%REPORT%" 2>&1
rustc --version >> "%REPORT%" 2>&1
cargo --version >> "%REPORT%" 2>&1
>> "%REPORT%" echo ```

rem ----------------------------------------------------------------- fetch --
echo   == source
>> "%REPORT%" echo.
>> "%REPORT%" echo ## Source
>> "%REPORT%" echo.
>> "%REPORT%" echo ```
if exist "%WORKDIR%\.git" (
  echo      updating existing checkout
  pushd "%WORKDIR%"
  git fetch origin %BRANCH% --depth 50 >> "%REPORT%" 2>&1
  git checkout -B %BRANCH% origin/%BRANCH% >> "%REPORT%" 2>&1
) else (
  echo      cloning
  git clone --branch %BRANCH% --depth 50 "%REPO%" "%WORKDIR%" >> "%REPORT%" 2>&1
  pushd "%WORKDIR%"
)
git log -1 --format="%%H %%an %%ad %%s" --date=short >> "%REPORT%" 2>&1
>> "%REPORT%" echo ```

rem ----------------------------------------------------------------- build --
echo   == build (this takes a while)
>> "%REPORT%" echo.
>> "%REPORT%" echo ## Build
>> "%REPORT%" echo.
>> "%REPORT%" echo ```
cargo build --workspace >> "%REPORT%" 2>&1
if errorlevel 1 (
  echo      FAILED
  >> "%REPORT%" echo ```
  >> "%REPORT%" echo **cargo build FAILED - see above.**
  goto :eyes
)
>> "%REPORT%" echo ```
>> "%REPORT%" echo OK.

rem ----------------------------------------------------------------- tests --
echo   == tests
>> "%REPORT%" echo.
>> "%REPORT%" echo ## Tests
>> "%REPORT%" echo.
>> "%REPORT%" echo ```
cargo test --workspace >> "%REPORT%" 2>&1
if errorlevel 1 (
  echo      FAILED
  >> "%REPORT%" echo ```
  >> "%REPORT%" echo **cargo test FAILED - see above.**
) else (
  >> "%REPORT%" echo ```
  >> "%REPORT%" echo OK.
)

echo   == pseudo-terminal tests
>> "%REPORT%" echo.
>> "%REPORT%" echo ## Pseudo-terminal (ConPTY)
>> "%REPORT%" echo.
>> "%REPORT%" echo ```
cargo test -p zyris-code --test pty -- --nocapture >> "%REPORT%" 2>&1
if errorlevel 1 (
  echo      FAILED
  >> "%REPORT%" echo ```
  >> "%REPORT%" echo **The ConPTY tests FAILED - this is the interesting one.**
) else (
  >> "%REPORT%" echo ```
  >> "%REPORT%" echo OK.
)

echo   == clippy
>> "%REPORT%" echo.
>> "%REPORT%" echo ## Clippy
>> "%REPORT%" echo.
>> "%REPORT%" echo ```
cargo clippy --workspace --all-targets >> "%REPORT%" 2>&1
>> "%REPORT%" echo ```

rem ------------------------------------------------------------ capability --
echo   == terminal capability
>> "%REPORT%" echo.
>> "%REPORT%" echo ## What this terminal looks like from inside the app
>> "%REPORT%" echo.
>> "%REPORT%" echo ```
cargo run --quiet --example term_report >> "%REPORT%" 2>&1
>> "%REPORT%" echo ```

:eyes
echo.
echo   ------------------------------------------------------------------
echo   Look at the rows below rather than reading them. Every ']' should
echo   sit in the SAME column. One further right means that character is
echo   drawn two columns wide here, which shifts everything after it and
echo   is the main reason this app breaks on some terminals.
echo   ------------------------------------------------------------------
echo.
cargo run --quiet --example term_report 2>nul | findstr /C:"]"
echo.
popd

rem ------------------------------------------------------------- checklist --
>> "%REPORT%" echo.
>> "%REPORT%" echo ## Checklist - please fill in
>> "%REPORT%" echo.
>> "%REPORT%" echo These need a person and a real terminal. Replace [ ] with [x] or [FAIL],
>> "%REPORT%" echo and add a line under anything that failed.
>> "%REPORT%" echo.
>> "%REPORT%" echo Terminal used: (Windows Terminal / cmd.exe / PowerShell window / other)
>> "%REPORT%" echo.
>> "%REPORT%" echo - [ ] The app starts and draws a screen (cargo run -p zyris-code)
>> "%REPORT%" echo - [ ] Typing shows in the input box, Backspace deletes
>> "%REPORT%" echo - [ ] Ctrl+U clears back to the start, Ctrl+W deletes a word, Ctrl+Y puts it back
>> "%REPORT%" echo - [ ] Shift+Enter makes a newline (if not, Alt+Enter must)
>> "%REPORT%" echo - [ ] Arrow keys and Home/End move the cursor
>> "%REPORT%" echo - [ ] Shift+Tab cycles the mode shown at the bottom left
>> "%REPORT%" echo - [ ] Typing / opens the command list and typing narrows it
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
>> "%REPORT%" echo - [ ] zyris-code -p "say hello" prints an answer and exits
>> "%REPORT%" echo.
>> "%REPORT%" echo If something failed, set one of these and say whether it changed:
>> "%REPORT%" echo.
>> "%REPORT%" echo     set ZYRIS_CODE_MOUSE=0          hand selection back to the terminal
>> "%REPORT%" echo     set ZYRIS_CODE_HYPERLINKS=0     stop sending OSC 8
>> "%REPORT%" echo     set ZYRIS_CODE_OSC52=0          stop sending clipboard writes
>> "%REPORT%" echo     set ZYRIS_CODE_HEAL_MS=300      redraw more often, for leftover characters

:done
echo.
echo   Done. Report written to:
echo   %REPORT%
echo.
echo   Please fill in the checklist at the end of it and send the file back.
echo.
pause
