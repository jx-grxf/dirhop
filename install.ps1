# dirhop installer for Windows
#   irm https://raw.githubusercontent.com/jx-grxf/dirhop/main/install.ps1 | iex
# Set $env:DIRHOP_INSTALL_DIR to install somewhere other than %LOCALAPPDATA%\Programs\dirhop.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$repo = 'jx-grxf/dirhop'
$dir = if ($env:DIRHOP_INSTALL_DIR) { $env:DIRHOP_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\dirhop' }
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'aarch64' } else { 'x86_64' }
$target = "$arch-pc-windows-msvc"
$asset = "dirhop-$target.zip"
$base = "https://github.com/$repo/releases/latest/download"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("dirhop-" + [guid]::NewGuid())
$exe = Join-Path $dir 'dirhop.exe'

New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    Write-Host "Downloading dirhop ($target)..."
    Invoke-WebRequest -UseBasicParsing "$base/$asset" -OutFile "$tmp\$asset"
    Invoke-WebRequest -UseBasicParsing "$base/$asset.sha256" -OutFile "$tmp\$asset.sha256"
    $expected = ((Get-Content "$tmp\$asset.sha256" -Raw).Trim() -split '\s+')[0].ToLower()
    $actual = (Get-FileHash "$tmp\$asset" -Algorithm SHA256).Hash.ToLower()
    if ($expected -ne $actual) { throw 'dirhop: checksum mismatch, aborting.' }

    Expand-Archive "$tmp\$asset" -DestinationPath "$tmp\out" -Force
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    # A running dirhop.exe can't be overwritten, only moved aside.
    if (Test-Path $exe) { Move-Item -Force $exe (Join-Path $dir '.dirhop.old.exe') }
    Copy-Item "$tmp\out\dirhop.exe" $exe
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
Write-Host "Installed $(& $exe --version) to $exe"

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not (($userPath -split ';') -contains $dir)) {
    [Environment]::SetEnvironmentVariable('Path', (($userPath.TrimEnd(';'), $dir) -join ';').TrimStart(';'), 'User')
    $env:Path += ";$dir"
    Write-Host "Added $dir to your PATH."
}

& $exe setup
