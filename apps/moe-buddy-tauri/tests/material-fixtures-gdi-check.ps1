# Optional Windows-only cross-check for the C synthetic material fixtures.
#
# Decodes the generated PNG/GIF files with the system GDI+ (System.Drawing) and
# compares them pixel by pixel with the expectations written by
# `tests/material-fixtures.mjs` into `expected/*.json`. It proves that the fixture
# files are readable by a real decoder; it is NOT Rust host decoding/playback
# acceptance and not a claim about the product decoder.
#
# Known policy difference: GDI+ fills the disposal-2 rectangle with the LSD
# background colour (opaque), while the fixtures require clear-to-transparent
# (WebView2/Chromium semantics for a transparent pet window). Those pixels are
# counted separately as `policy-diff`, never as a hard failure.
#
# This file is intentionally ASCII-only: Windows PowerShell 5.1 reads .ps1 files
# as ANSI unless they have a BOM, which would corrupt non-ASCII text.
#
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File tests/material-fixtures-gdi-check.ps1 [-Out <dir>]

param(
    [string]$Out = ''
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

function Read-Utf8Json([string]$Path) {
    return [System.IO.File]::ReadAllText($Path, [System.Text.Encoding]::UTF8) | ConvertFrom-Json
}

if ([string]::IsNullOrWhiteSpace($Out)) {
    $dir = $PSScriptRoot
    while ($true) {
        if ((Test-Path (Join-Path $dir 'Cargo.toml')) -and (Test-Path (Join-Path $dir 'AGENTS.md'))) { break }
        $parent = Split-Path $dir -Parent
        if ([string]::IsNullOrEmpty($parent)) { throw 'cannot locate repository root' }
        $dir = $parent
    }
    $Out = Join-Path $dir 'target/c-material-fixtures'
}
$Out = (Resolve-Path $Out).Path
$indexPath = Join-Path $Out 'index.json'
if (-not (Test-Path $indexPath)) { throw "missing $indexPath; run node tests/material-fixtures.mjs first" }

$index = Read-Utf8Json $indexPath
$exact = 0
$policy = 0
$failed = 0
$checked = 0
$digestOnly = 0

function Compare-Bitmap($Bitmap, [byte[]]$Want, [string]$Label, [bool]$AllowPolicy) {
    $bad = 0
    $policyBad = 0
    $first = $null
    for ($y = 0; $y -lt $Bitmap.Height; $y++) {
        for ($x = 0; $x -lt $Bitmap.Width; $x++) {
            $c = $Bitmap.GetPixel($x, $y)
            $o = ($y * $Bitmap.Width + $x) * 4
            if ($c.R -ne $Want[$o] -or $c.G -ne $Want[$o + 1] -or $c.B -ne $Want[$o + 2] -or $c.A -ne $Want[$o + 3]) {
                if ($AllowPolicy -and $Want[$o + 3] -eq 0 -and $c.A -eq 255) {
                    $policyBad++
                } else {
                    $bad++
                    if ($null -eq $first) {
                        $first = "($x,$y) got=$($c.R),$($c.G),$($c.B),$($c.A) want=$($Want[$o]),$($Want[$o + 1]),$($Want[$o + 2]),$($Want[$o + 3])"
                    }
                }
            }
        }
    }
    return [pscustomobject]@{ Bad = $bad; Policy = $policyBad; First = $first; Label = $Label }
}

foreach ($entry in $index.fixtures | Where-Object { $_.expect -eq 'accept' }) {
    $package = Join-Path $Out $entry.packageDir
    $manifest = Read-Utf8Json (Join-Path $package 'manifest.json')
    $expected = Read-Utf8Json (Join-Path $Out $entry.expectedFile)
    foreach ($clip in $manifest.clips) {
        $clipExpected = $expected.clips.($clip.id)
        $allowPolicy = $null -ne $clipExpected.disposalPolicy
        if ($clip.type -eq 'sequence') {
            for ($i = 0; $i -lt $clip.frames.Count; $i++) {
                if ($null -eq $clipExpected.frames[$i].rgbaBase64) {
                    # The sidecar only keeps sha256 for repeated frames (checked by material-fixtures-verify.mjs).
                    $digestOnly++
                    continue
                }
                $path = Join-Path $package ($clip.frames[$i].path -replace '/', '\')
                $bmp = [System.Drawing.Bitmap]::FromFile($path)
                try {
                    $want = [Convert]::FromBase64String($clipExpected.frames[$i].rgbaBase64)
                    $result = Compare-Bitmap $bmp $want "$($entry.name)/$($clip.id)/frame$i" $false
                } finally { $bmp.Dispose() }
                $checked++
                if ($result.Bad -gt 0) { $failed++; "FAIL $($result.Label) $($result.First)" } else { $exact++ }
            }
        } else {
            $path = Join-Path $package ($clip.path -replace '/', '\')
            $image = [System.Drawing.Image]::FromFile($path)
            try {
                $dimension = New-Object System.Drawing.Imaging.FrameDimension($image.FrameDimensionsList[0])
                $frameCount = $image.GetFrameCount($dimension)
                if ($frameCount -ne $clipExpected.frameCount) {
                    $failed++
                    "FAIL $($entry.name)/$($clip.id) GDI+ frame count $frameCount != expected $($clipExpected.frameCount)"
                }
                for ($i = 0; $i -lt $frameCount; $i++) {
                    $image.SelectActiveFrame($dimension, $i) | Out-Null
                    $bmp = New-Object System.Drawing.Bitmap($image)
                    try {
                        $want = [Convert]::FromBase64String($clipExpected.frames[$i].rgbaBase64)
                        $result = Compare-Bitmap $bmp $want "$($entry.name)/$($clip.id)/frame$i" $allowPolicy
                    } finally { $bmp.Dispose() }
                    $checked++
                    if ($result.Bad -gt 0) {
                        $failed++
                        "FAIL $($result.Label) $($result.First)"
                    } elseif ($result.Policy -gt 0) {
                        $policy++
                        "policy $($result.Label) disposal2 background-policy pixels=$($result.Policy)"
                    } else {
                        $exact++
                    }
                }
            } finally { $image.Dispose() }
        }
    }
}

"GDI+ cross-check: checked=$checked exact=$exact policy-diff=$policy digest-only=$digestOnly failed=$failed"
if ($failed -gt 0) { exit 1 }
exit 0
