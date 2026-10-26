# Install psx from a GitHub release (Windows, x64):
#
#   powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/bombpersons/psx/master/install.ps1 | iex"
#
# Settings (environment variables):
#   PSX_VERSION=v0.2.0      a specific release instead of the latest
#   PSX_INSTALL_DIR=DIR     where psx.exe goes (default %LOCALAPPDATA%\psx\bin, added to your PATH)
#   PSX_NO_SIGNATURES=1     skip the PsyQ signatures (only `psx symbols` needs them)
#   PSX_DOWNLOAD_BASE=URL   fetch the archive from here instead (a mirror)
#
# Running it again updates psx and the signatures. Nothing else is downloaded:
# BIOS images and discs are yours to supply.
$ErrorActionPreference = 'Stop'

$repo = 'bombpersons/psx'
$sigsRepo = 'https://github.com/lab313ru/psx_psyq_signatures'
if (-not [Environment]::Is64BitOperatingSystem) { throw 'psx install: only 64-bit Windows has a prebuilt binary' }

$base = if ($env:PSX_DOWNLOAD_BASE) { $env:PSX_DOWNLOAD_BASE } elseif ($env:PSX_VERSION) { "https://github.com/$repo/releases/download/$env:PSX_VERSION" } else { "https://github.com/$repo/releases/latest/download" }
$asset = 'psx-x86_64-pc-windows-msvc.zip'
$data = Join-Path $env:LOCALAPPDATA 'psx'
$dir = if ($env:PSX_INSTALL_DIR) { $env:PSX_INSTALL_DIR } else { Join-Path $data 'bin' }

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("psx-install-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $label = if ($env:PSX_VERSION) { $env:PSX_VERSION } else { 'latest release' }
    Write-Host "Downloading $asset ($label)"
    $ProgressPreference = 'SilentlyContinue'
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile (Join-Path $tmp $asset)
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset.sha256" -OutFile (Join-Path $tmp "$asset.sha256")
    $expected = ((Get-Content (Join-Path $tmp "$asset.sha256") -Raw).Trim() -split '\s+')[0].ToLower()
    $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp $asset)).Hash.ToLower()
    if ($expected -ne $actual) { throw "psx install: checksum mismatch for $asset" }

    Expand-Archive -Path (Join-Path $tmp $asset) -DestinationPath (Join-Path $tmp 'x')
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    Copy-Item -Force (Join-Path $tmp 'x\psx.exe') (Join-Path $dir 'psx.exe')
    # psx-clean is clean mode, for a port's implementer; older releases lack it.
    $clean = Join-Path $tmp 'x\psx-clean.exe'
    if (Test-Path $clean) { Copy-Item -Force $clean (Join-Path $dir 'psx-clean.exe') }
} finally {
    Remove-Item -Recurse -Force $tmp
}
$exe = Join-Path $dir 'psx.exe'
Write-Host "Installed $(& $exe --version) to $exe"

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not (($userPath -split ';') -contains $dir)) {
    [Environment]::SetEnvironmentVariable('Path', ($(if ($userPath) { "$userPath;" } else { '' }) + $dir), 'User')
    Write-Host "Added $dir to your user PATH (open a new terminal to use it)"
}

# PsyQ library signatures for `psx symbols`, in the shared data directory psx
# falls back to when a project has no external\psx_psyq_signatures.
$sigs = Join-Path $data 'psyq_signatures'
if ($env:PSX_NO_SIGNATURES) {
} elseif (-not (Get-Command git -ErrorAction SilentlyContinue)) {
    Write-Host 'git not found: skipped the PsyQ signatures (only psx symbols needs them)'
} elseif (Test-Path (Join-Path $sigs '.git')) {
    git -C $sigs pull -q --ff-only
    if ($LASTEXITCODE -eq 0) { Write-Host "Updated PsyQ signatures in $sigs" } else { Write-Host "Could not update $sigs" }
} else {
    Write-Host "Downloading PsyQ signatures to $sigs"
    git clone -q --depth 1 $sigsRepo $sigs
    if ($LASTEXITCODE -ne 0) { Write-Host 'Could not download the PsyQ signatures (only psx symbols needs them)' }
}

Write-Host ''
Write-Host 'Next, add a PS1 BIOS (your own dump, or a checksum-verified download):'
Write-Host '  psx bios install --from C:\path\to\bios.bin'
Write-Host '  psx bios install --download --region pal       # or ntsc-u, ntsc-j'
Write-Host 'Then use it from Claude Code:'
Write-Host "  claude mcp add --scope user psx -- `"$exe`" mcp"
