# Does install.ps1 replace a binary that is being run, twice over?
#
# **The second update is the whole point.** The first moves the running .exe aside and the new one
# takes its name; the window that updated then restarts on it, so the name is locked as well --
# and the file moved aside is still being executed by whichever window never restarted. A fixed
# name to move aside is free only the first time, and the second update then fails on the name it
# was never asked to touch:
#
#     Copy-Item : 'zyris.exe' cannot be accessed, it is in use by another process
#
# which is what was reported on 2026-08-18. It reads like a permissions problem and is not one.
#
# Run it with:  pwsh -NoProfile -File scripts/installer_smoke.ps1
#
# **Nothing is downloaded and nothing installed.** The function is taken out of install.ps1 by
# parsing the file, so this cannot drift from what ships, and everything it touches is a copy of
# ping.exe in a temporary directory.
$ErrorActionPreference = 'Stop'
# The installer beside this script, so it runs from a checkout wherever that checkout is.
$path = Join-Path (Split-Path (Split-Path $PSCommandPath)) 'install.ps1'

# Parse first -- this is what CI does, and a script that does not parse installs nothing.
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile($path, [ref]$null, [ref]$errors)
if ($errors) { Write-Host "PARSE FAIL: $($errors[0])"; exit 1 }
Write-Host 'parse ok'

# The real function out of the real file, so this cannot drift from what ships.
$fn = $ast.Find({ param($n)
  $n -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq 'Install-Binary'
}, $true)
if (-not $fn) { Write-Host 'FAIL: Install-Binary is not in install.ps1'; exit 1 }
Invoke-Expression $fn.Extent.Text

$dir = Join-Path $env:TEMP ("instest-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $dir | Out-Null
$target = Join-Path $dir 'zyris.exe'
$running = @()
try {
    # Three builds that really run. Trailing bytes past the PE image are ignored by the loader,
    # so these are the same program with three different hashes -- which is what tells the
    # versions apart without needing three separate programs.
    $base = [IO.File]::ReadAllBytes('C:\Windows\System32\PING.EXE')
    $builds = @{}
    foreach ($v in 1..3) {
        $file = Join-Path $dir "v$v.bin"
        [IO.File]::WriteAllBytes($file, ($base + [byte[]]@($v, $v, $v)))
        $builds["v$v"] = @{ Path = $file; Hash = (Get-FileHash $file -Algorithm SHA256).Hash }
    }
    $hashOfTarget = { (Get-FileHash $target -Algorithm SHA256).Hash }
    $start = {
        $p = Start-Process -FilePath $target -ArgumentList '-n','300','127.0.0.1' -PassThru -WindowStyle Hidden
        Start-Sleep -Milliseconds 500
        $script:running += $p
        $p
    }

    # The first window, on v1.
    Copy-Item $builds['v1'].Path $target
    & $start | Out-Null

    # It updates itself. The file it is executing gets moved aside; the new one takes the name.
    Install-Binary $builds['v2'].Path $target
    if ((& $hashOfTarget) -ne $builds['v2'].Hash) { Write-Host 'FAIL: the first update did not land'; exit 1 }
    Write-Host 'v2 landed over the copy v1 is running'

    # **The part the earlier attempt missed.** The updated window restarts on the new binary, so
    # the name is locked as well now -- and the file moved aside is still being executed by the
    # window that never restarted. That is the reported state exactly.
    & $start | Out-Null

    Install-Binary $builds['v3'].Path $target
    if ((& $hashOfTarget) -ne $builds['v3'].Hash) { Write-Host 'FAIL: the second update did not land'; exit 1 }
    Write-Host 'v3 landed with one window on the name and an older one on what was moved aside'

    $aside = Get-ChildItem -Path $dir -Filter 'zyris.exe.old*' -File | Sort-Object Name
    Write-Host ("moved aside: " + (($aside | ForEach-Object { $_.Name }) -join ', '))

    # Once nothing holds them, the next install sweeps them, so they do not pile up. Exactly one
    # is left: the copy that install just replaced, which is the point of moving it aside.
    foreach ($p in $running) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Milliseconds 800
    Install-Binary $builds['v1'].Path $target
    $left = @(Get-ChildItem -Path $dir -Filter 'zyris.exe.old*' -File)
    Write-Host ("left behind once every window closed: " + (($left | ForEach-Object { $_.Name }) -join ', '))
    if ($left.Count -ne 1 -or $left[0].Name -ne 'zyris.exe.old') {
        Write-Host 'FAIL: what was moved aside piled up'; exit 1
    }
    Write-Host 'ALL OK'
} catch {
    Write-Host "FAIL: $($_.Exception.Message)"
    exit 1
} finally {
    foreach ($p in $running) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Milliseconds 400
    Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
}
