# spotcli installer for Windows
# Installs spotcli to %LOCALAPPDATA%\Programs\spotcli and adds it to your user PATH.
# Run with:  powershell -ExecutionPolicy Bypass -File install.ps1

$ErrorActionPreference = 'Stop'

$installDir = Join-Path $env:LOCALAPPDATA 'Programs\spotcli\bin'
$exe = Join-Path $PSScriptRoot 'spotcli.exe'

if (-not (Test-Path $exe)) {
    Write-Error "spotcli.exe not found next to this script ($PSScriptRoot)."
    exit 1
}

# Stop a running instance so the file can be replaced
Get-Process -Name spotcli -ErrorAction SilentlyContinue | ForEach-Object {
    Write-Host "Stopping running spotcli (PID $($_.Id))"
    try { $_.Kill() } catch {}
}

New-Item -ItemType Directory -Force -Path $installDir | Out-Null
Copy-Item -Force $exe (Join-Path $installDir 'spotcli.exe')
Write-Host "Installed to $installDir"

# Add to user PATH if not already there
$currentPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($null -eq $currentPath) { $currentPath = '' }
$entries = $currentPath.Split(';') | Where-Object { $_ -ne '' }
if ($entries -notcontains $installDir) {
    $newPath = if ($currentPath.TrimEnd(';') -eq '') { $installDir } else { $currentPath.TrimEnd(';') + ';' + $installDir }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    Write-Host "Added $installDir to your user PATH"
} else {
    Write-Host 'PATH already contains the install directory'
}

# Verify the installed binary
$installed = Join-Path $installDir 'spotcli.exe'
$version = & $installed --version
Write-Host "Installed $version"
Write-Host ''
Write-Host 'Done. Open a NEW terminal and run:  spotcli'
Write-Host 'First time? Run:  spotcli login --client-id YOUR_SPOTIFY_CLIENT_ID'
