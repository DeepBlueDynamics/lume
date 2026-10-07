# Install the latest Lume release (or $env:LUME_VERSION) on Windows x64.
#
#   irm https://github.com/DeepBlueDynamics/lume/releases/latest/download/install.ps1 | iex
#
# Environment:
#   LUME_VERSION      tag to install, e.g. v0.12.1 (default: latest release)
#   LUME_INSTALL_DIR  destination directory (default: %LOCALAPPDATA%\Programs\lume)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$repo = 'DeepBlueDynamics/lume'
$installDir = if ($env:LUME_INSTALL_DIR) { $env:LUME_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\lume' }

if (-not [Environment]::Is64BitOperatingSystem) { throw 'lume install: only 64-bit Windows is supported' }
$target = 'x86_64-pc-windows-msvc'

$tag = $env:LUME_VERSION
if (-not $tag) {
    $tag = (Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest").tag_name
    if (-not $tag) { throw "lume install: could not find the latest release of $repo" }
}

$name = "lume-$tag-$target"
$base = "https://github.com/$repo/releases/download/$tag"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("lume-install-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    Write-Host "Installing lume $tag ($target) into $installDir"
    $zip = Join-Path $tmp "$name.zip"
    Invoke-WebRequest "$base/$name.zip" -OutFile $zip
    $sums = Invoke-WebRequest "$base/SHA256SUMS" -UseBasicParsing
    $line = ($sums.Content -split "`n") | Where-Object { $_ -match "\s$([regex]::Escape("$name.zip"))\s*$" } | Select-Object -First 1
    if (-not $line) { throw "lume install: $name.zip is not listed in SHA256SUMS" }
    $expected = ($line -split '\s+')[0].ToLower()
    $actual = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
    if ($expected -ne $actual) { throw "lume install: checksum mismatch for $name.zip" }

    Expand-Archive $zip -DestinationPath $tmp
    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    Copy-Item (Join-Path $tmp "$name\lume.exe") (Join-Path $installDir 'lume.exe') -Force
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not (($userPath -split ';') -contains $installDir)) {
    $newPath = if ($userPath) { "$userPath;$installDir" } else { $installDir }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    $env:Path = "$env:Path;$installDir"
    Write-Host "Added $installDir to your user PATH (open a new terminal to pick it up)."
}
Write-Host "Installed $(& (Join-Path $installDir 'lume.exe') --version)"
