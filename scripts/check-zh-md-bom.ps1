param(
    [string]$Root = "."
)

$ErrorActionPreference = "Stop"
$rootPath = (Resolve-Path $Root).Path

$files = Get-ChildItem -Path $rootPath -Recurse -File -Filter "*.zh-CN.md" |
    Where-Object { $_.FullName -notmatch "\\.git\\|\\target\\|\\node_modules\\|\\.venv\\|\\dist\\|\\build\\" }

if (-not $files) {
    Write-Host "No *.zh-CN.md files found."
    exit 0
}

$missing = @()

foreach ($file in $files) {
    $bytes = [System.IO.File]::ReadAllBytes($file.FullName)
    $hasBom = $bytes.Length -ge 3 -and
        $bytes[0] -eq 0xEF -and
        $bytes[1] -eq 0xBB -and
        $bytes[2] -eq 0xBF

    if (-not $hasBom) {
        $missing += $file.FullName
    }
}

if ($missing.Count -gt 0) {
    Write-Host "FAIL: UTF-8 BOM missing in these files:" -ForegroundColor Red
    foreach ($path in $missing) {
        Write-Host "  $path"
    }
    exit 1
}

Write-Host "PASS: all *.zh-CN.md files are UTF-8 with BOM." -ForegroundColor Green
exit 0
