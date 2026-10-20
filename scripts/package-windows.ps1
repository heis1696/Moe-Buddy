param(
    [switch]$SkipBuild
)
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'This package script requires Windows.' }
if ($env:CARGO_BUILD_TARGET -or $env:TAURI_CONFIG) { throw 'Clear build target/config overrides before packaging.' }
$root = Split-Path $PSScriptRoot -Parent
$frontend = Join-Path $root 'apps/moe-buddy-tauri'
$cargo = Join-Path $env:USERPROFILE '.cargo/bin/cargo.exe'
if (!(Test-Path -LiteralPath $cargo -PathType Leaf)) { $cargo = 'cargo' }
Push-Location $root
try {
    if (!$SkipBuild) {
        & npm.cmd --prefix $frontend run build
        if ($LASTEXITCODE -ne 0) { throw 'Frontend build failed.' }
        & $cargo build -p moe-buddy-tauri --release --locked --offline
        if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }
    }
    $binary = Join-Path $root 'target/release/moe-buddy-tauri.exe'
    if (!(Test-Path -LiteralPath $binary -PathType Leaf)) { throw 'Release binary missing.' }
    $binaryBytes = [IO.File]::ReadAllBytes($binary)
    $peOffset = [BitConverter]::ToInt32($binaryBytes, 0x3c)
    if ([BitConverter]::ToUInt16($binaryBytes, $peOffset + 4) -ne 0x8664) { throw 'Expected an x64 executable.' }
    $versionLine = Get-Content -LiteralPath (Join-Path $root 'Cargo.toml') | Where-Object { $_ -match '^version = "([0-9.]+)"$' } | Select-Object -First 1
    if ($versionLine -notmatch '^version = "([0-9.]+)"$') { throw 'Workspace version missing.' }
    $version = $Matches[1]
    $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $name = "Moe-Buddy-$version-windows-x64-$stamp"
    $out = Join-Path $root "releases/$name"
    if (Test-Path -LiteralPath $out) { throw 'Package output already exists; refusing overwrite.' }
    New-Item -ItemType Directory -Path $out | Out-Null
    Copy-Item -LiteralPath $binary -Destination (Join-Path $out 'Moe Buddy.exe')
    [IO.File]::WriteAllText((Join-Path $out 'moe-buddy.portable'), "1`n", [Text.UTF8Encoding]::new($false))
    Copy-Item -LiteralPath (Join-Path $frontend 'portable/README.txt') -Destination (Join-Path $out 'README.txt')
    $commit = (git rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read source revision.' }
    $changes = @(git status --porcelain --untracked-files=normal)
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read source status.' }
    $hash = (Get-FileHash -LiteralPath (Join-Path $out 'Moe Buddy.exe') -Algorithm SHA256).Hash.ToLowerInvariant()
    $info = [ordered]@{
        product = 'Moe Buddy'; version = $version; platform = 'windows-x64';
        sourceCommit = $commit; sourceDirty = ($changes.Count -gt 0);
        packagedAt = (Get-Date).ToUniversalTime().ToString('o');
        binarySha256 = $hash; rebuilt = (!$SkipBuild);
        requirements = 'Windows x64; Microsoft Edge WebView2 Evergreen Runtime';
        data = 'data/ (created on first launch; no existing user data included)'
    }
    [IO.File]::WriteAllText((Join-Path $out 'BUILD-INFO.json'), ($info | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
    $zip = "$out.zip"
    Compress-Archive -LiteralPath $out -DestinationPath $zip -CompressionLevel Optimal
    $zipHash = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText("$zip.sha256", "$zipHash  $name.zip`n", [Text.UTF8Encoding]::new($false))
    Write-Output "Package: $zip"
    Write-Output "SHA256:  $zipHash"
} finally {
    Pop-Location
}
