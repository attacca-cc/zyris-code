<#
Install zyris-code on Windows.

    irm https://github.com/attacca-cc/zyris-code/releases/latest/download/install.ps1 | iex

To pass options, download it first:

    irm https://github.com/attacca-cc/zyris-code/releases/latest/download/install.ps1 -OutFile install.ps1
    .\install.ps1 -Version v0.1.0

**Nothing here needs administrator rights.** It installs under your own profile and edits only
your own PATH.
#>
[CmdletBinding()]
param(
    # Install this release instead of the newest.
    [string] $Version,
    # Install here instead of %LOCALAPPDATA%\Programs\zyris-code.
    [string] $Dir = $(if ($env:ZYRIS_CODE_INSTALL_DIR) { $env:ZYRIS_CODE_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\zyris-code' }),
    # Leave PATH alone.
    [switch] $NoModifyPath
)

$ErrorActionPreference = 'Stop'

# A self-update hands us the install directory it read with Rust's canonicalize, which on Windows
# is the extended-length form (\\?\C:\...). PowerShell's path cmdlets cannot parse that prefix:
# Join-Path fails with "the value of argument 'drive' is null" and the install dies at exit 1
# before the binary is placed, so the update never takes and every launch tries again. Bring it
# back to an ordinary path. Written to tolerate old binaries too — they fetch this script fresh.
if ($Dir) {
    if ($Dir.StartsWith('\\?\UNC\')) { $Dir = '\\' + $Dir.Substring('\\?\UNC\'.Length) }
    elseif ($Dir.StartsWith('\\?\')) { $Dir = $Dir.Substring('\\?\'.Length) }
}

# Without this, older PowerShell hosts negotiate a TLS version GitHub no longer accepts and the
# download fails with something that reads like a network fault.
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Repo  = 'attacca-cc/zyris-code'
$Bin   = 'zyris-code'
# The short name people actually type. Both end up on PATH.
$Alias = 'zyris'

# ── Which build ──────────────────────────────────────────────────────────────
$arch = switch ($env:PROCESSOR_ARCHITECTURE) {
    'AMD64' { 'x86_64' }
    'ARM64' { 'aarch64' }
    default { $null }
}
if (-not $arch) {
    throw "unsupported architecture: $env:PROCESSOR_ARCHITECTURE"
}
$target  = "$arch-pc-windows-msvc"
$archive = "$Bin-$target.zip"

$base = if ($Version) {
    "https://github.com/$Repo/releases/download/$Version"
} else {
    "https://github.com/$Repo/releases/latest/download"
}

# ── Fetch ────────────────────────────────────────────────────────────────────
#
# **The archive is fetched by hand so it can say how far along it is.** `Invoke-WebRequest` has a
# progress display of its own, but in Windows PowerShell it is a full-width banner drawn over the
# top of the console and it costs more time than it explains — measurably several times the
# download itself, because the bar is redrawn per read. Reading the stream here means one line,
# rewritten in place, and no such cost.
#
# Everything smaller stays on `Invoke-WebRequest`: a bar for a one-line checksum file is noise.
function Get-Archive {
    param([string] $Url, [string] $Path)

    # **Only drawn for somebody watching.** Redirected to a file or a build log, `\r` does not
    # return anywhere and every update becomes another line.
    $watching = -not [Console]::IsOutputRedirected

    $response = ([Net.WebRequest]::Create($Url)).GetResponse()
    $total = $response.ContentLength
    $in = $response.GetResponseStream()
    $out = [IO.File]::Create($Path)
    try {
        $buffer = New-Object byte[] 131072
        $done = 0
        $shown = -1
        while (($read = $in.Read($buffer, 0, $buffer.Length)) -gt 0) {
            $out.Write($buffer, 0, $read)
            $done += $read
            # Redrawn when the number changes, not per read: at 128 KiB a chunk that is hundreds
            # of writes for the same picture.
            if ($watching -and $total -gt 0) {
                $percent = [int](100 * $done / $total)
                if ($percent -ne $shown) {
                    $shown = $percent
                    $filled = [int](28 * $percent / 100)
                    # `#` and `.`, because a block-drawing character is a box in half the fonts a
                    # Windows console ships with.
                    $bar = ('#' * $filled) + ('.' * (28 - $filled))
                    $size = '{0,6:N1}/{1:N1} MB' -f ($done / 1MB), ($total / 1MB)
                    Write-Host ("`r  [{0}] {1,3}%  {2}" -f $bar, $percent, $size) -NoNewline
                }
            }
        }
        if ($shown -ge 0) { Write-Host '' }
    } finally {
        $out.Close()
        $in.Close()
        $response.Close()
    }
}

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("zyris-code-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp -Force | Out-Null
try {
    Write-Host "downloading $archive"
    try {
        Get-Archive -Url "$base/$archive" -Path (Join-Path $tmp $archive)
    } catch {
        throw "no build for $target in that release. See https://github.com/$Repo/releases"
    }

    # **Check the download before unpacking it.** This is a binary about to go on your PATH.
    $sums = Join-Path $tmp 'SHA256SUMS'
    try {
        Invoke-WebRequest -Uri "$base/SHA256SUMS" -OutFile $sums -UseBasicParsing
    } catch {
        $sums = $null
        Write-Warning 'SHA256SUMS is missing from this release, skipping the checksum'
    }
    if ($sums) {
        $want = (Get-Content $sums | Where-Object { $_ -match "\s$([regex]::Escape($archive))$" } |
                 ForEach-Object { ($_ -split '\s+')[0] } | Select-Object -First 1)
        if (-not $want) { throw "$archive is not listed in SHA256SUMS" }
        $got = (Get-FileHash -Path (Join-Path $tmp $archive) -Algorithm SHA256).Hash
        if ($got -ne $want.ToUpperInvariant()) {
            throw "checksum mismatch for $archive - refusing to install"
        }
        Write-Host 'checksum ok'
    }

    # ── Install ──────────────────────────────────────────────────────────────
    Expand-Archive -Path (Join-Path $tmp $archive) -DestinationPath $tmp -Force
    $exe = Join-Path $tmp "$Bin.exe"
    if (-not (Test-Path $exe)) { throw "the archive did not contain $Bin.exe" }

    New-Item -ItemType Directory -Path $Dir -Force | Out-Null

    # **A running .exe cannot be overwritten on Windows, but it can be renamed.** The handle
    # follows the file, so moving it aside frees the name at once and the process that is using
    # it carries on undisturbed. Overwriting in place is what fails with "the process cannot
    # access the file" - which reads like a permissions problem and is not one.
    #
    # This is what lets zyris-code update itself. Without it a self-update could only ever tell
    # you to close the thing you were using.
    function Install-Binary($From, $To) {
        if (Test-Path $To) {
            $old = "$To.old"
            # Last update's leftover, now that nothing is holding it.
            Remove-Item $old -Force -ErrorAction SilentlyContinue
            try {
                Move-Item $To $old -Force
            } catch {
                # Renaming failed for some other reason; overwriting may still work.
            }
        }
        Copy-Item $From $To -Force
    }

    Install-Binary $exe (Join-Path $Dir "$Bin.exe")
    # The short name. Windows has no usable symlink without elevation, so this is a second copy.
    Install-Binary $exe (Join-Path $Dir "$Alias.exe")

    Write-Host "installed to $Dir"

    # ── PATH ─────────────────────────────────────────────────────────────────
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $onPath = ($userPath -split ';' | Where-Object { $_ -and ($_.TrimEnd('\') -ieq $Dir.TrimEnd('\')) })

    if ($onPath) {
        Write-Host ''
        Write-Host "Run it with:  $Alias"
    } elseif ($NoModifyPath) {
        Write-Host ''
        Write-Host "$Dir is not on your PATH. Add it yourself:"
        Write-Host "    [Environment]::SetEnvironmentVariable('Path', `"$Dir;`$env:Path`", 'User')"
    } else {
        # **User scope, not machine.** Machine scope needs elevation and would change PATH for
        # everyone on the box for the sake of one account's tool.
        $updated = if ([string]::IsNullOrEmpty($userPath)) { $Dir } else { "$userPath;$Dir" }
        [Environment]::SetEnvironmentVariable('Path', $updated, 'User')
        # The line above only reaches processes started afterwards, so this session gets it too —
        # otherwise `zyris` fails right after an install that said it succeeded.
        $env:Path = "$env:Path;$Dir"
        Write-Host ''
        Write-Host "Added $Dir to your PATH."
        Write-Host 'Open a new terminal to pick it up everywhere.'
        Write-Host ''
        Write-Host "Then run:  $Alias"
    }
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
