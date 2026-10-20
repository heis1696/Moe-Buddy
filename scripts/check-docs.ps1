$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$utf8 = [System.Text.UTF8Encoding]::new($false, $true)
# Use repository-owned files so installed frontend packages and generated output
# do not become project documentation. Include new, untracked source documents.
$repoFiles = @(git -C $root ls-files --cached --others --exclude-standard)
if ($LASTEXITCODE -ne 0) { throw 'Unable to enumerate repository documentation.' }
$files = @($repoFiles | Where-Object {
    $_ -match '^(?:AGENTS\.md|README\.md|docs/[^/]+\.md|(?:crates|apps)/.+/README\.md)$'
} | Sort-Object -Unique | ForEach-Object {
    $docPath = Join-Path $root $_
    if (Test-Path -LiteralPath $docPath -PathType Leaf) { Get-Item -LiteralPath $docPath }
})
$issues = [System.Collections.Generic.List[string]]::new()
foreach ($file in $files) {
    try {
        $text = $utf8.GetString([System.IO.File]::ReadAllBytes($file.FullName))
    } catch {
        $issues.Add("$($file.Name): invalid UTF-8")
        continue
    }
    if ($text.Contains([char]0xfffd)) { $issues.Add("$($file.Name): replacement character") }
    if ($text -notmatch '\r?\n$') { $issues.Add("$($file.Name): missing final newline") }
    if ($text -match '(?m)[ \t]+\r?$') { $issues.Add("$($file.Name): trailing whitespace") }
    if ($text -match '\t') { $issues.Add("$($file.Name): tab whitespace") }
    if ($text -match '(?:\r?\n){3,}$') { $issues.Add("$($file.Name): excess final blank lines") }
    $fence = $null
    foreach ($line in ($text -split '\r?\n')) {
        if ($line -match '^\s*(`{3,}|~{3,})') {
            $marker = $Matches[1]
            if ($null -eq $fence) { $fence = $marker }
            elseif ($marker[0] -eq $fence[0] -and $marker.Length -ge $fence.Length) { $fence = $null }
        }
    }
    if ($null -ne $fence) { $issues.Add("$($file.Name): unclosed fence") }
    foreach ($link in [regex]::Matches($text, '\[[^\]]*\]\((?<target>[^)]+)\)')) {
        $target = $link.Groups['target'].Value
        if ($target -match '^[a-zA-Z][a-zA-Z0-9+.-]*:') { continue }
        $parts = $target -split '#', 2
        $targetPath = $file.FullName
        if ($parts[0]) { $targetPath = [System.IO.Path]::GetFullPath((Join-Path $file.DirectoryName ([uri]::UnescapeDataString($parts[0])))) }
        if (-not (Test-Path -LiteralPath $targetPath -PathType Leaf)) {
            $issues.Add("$($file.Name): missing link $target")
            continue
        }
        if ($parts.Count -gt 1 -and $parts[1]) {
            $targetText = $utf8.GetString([System.IO.File]::ReadAllBytes($targetPath))
            $headings = @([regex]::Matches($targetText, '(?m)^#{1,6}\s+([^\r\n]+)') | ForEach-Object {
                ($_.Groups[1].Value.ToLowerInvariant() -replace '[^\p{L}\p{N}\s_-]', '' -replace '\s', '-')
            })
            if ([uri]::UnescapeDataString($parts[1]) -notin $headings) { $issues.Add("$($file.Name): missing anchor $target") }
        }
    }
    foreach ($reference in [regex]::Matches($text, '`(?<path>(?:crates/|apps/|docs/)[^`\s]+)`')) {
        $relative = $reference.Groups['path'].Value -replace ':\d+(?::\d+)?$', ''
        if (-not (Test-Path -LiteralPath (Join-Path $root $relative))) { $issues.Add("$($file.Name): missing repo path $relative") }
    }
}
if ($issues.Count -gt 0) { throw ($issues -join [Environment]::NewLine) }
Write-Output "PASS: $($files.Count) active Markdown files; UTF-8, paths/anchors, fences and whitespace; archive excluded."
