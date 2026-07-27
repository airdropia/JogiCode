# ========================================================
# PRUNE-WINDOWS.PS1 — Windows Bundle Optimization
# ========================================================
$ErrorActionPreference = "Stop"

Write-Host "[Prune] Starting Windows bundle optimization..." -ForegroundColor Cyan

# Path to code-server inside Tauri bundle
$codeServerPath = "src-tauri/binaries/code-server"

# -------------------------------------------------------
# 1. Remove non-Windows ripgrep binaries
# -------------------------------------------------------
Write-Host "[1] Removing non-Windows ripgrep binaries..." -ForegroundColor Yellow
if (Test-Path "$codeServerPath/node_modules/vscode-ripgrep/bin") {
    Get-ChildItem -Path "$codeServerPath/node_modules/vscode-ripgrep/bin" -Recurse -Include "rg" -Exclude "*.exe" | Remove-Item -Force -Verbose
}

# -------------------------------------------------------
# 2. Remove macOS icon
# -------------------------------------------------------
Write-Host "[2] Removing macOS icon..." -ForegroundColor Yellow
Remove-Item -Path "src-tauri/icons/icon.icns" -Force -ErrorAction SilentlyContinue

# -------------------------------------------------------
# 3. Remove source maps & TypeScript types
# -------------------------------------------------------
Write-Host "[3] Removing source maps and TypeScript types..." -ForegroundColor Yellow
if (Test-Path $codeServerPath) {
    Get-ChildItem -Path $codeServerPath -Include "*.map","*.d.ts" -Recurse | Remove-Item -Force -Verbose
}

# -------------------------------------------------------
# 4. Remove C++ source artifacts
# -------------------------------------------------------
Write-Host "[4] Removing C++ source artifacts..." -ForegroundColor Yellow
if (Test-Path "$codeServerPath/node_modules") {
    Get-ChildItem -Path "$codeServerPath/node_modules" -Include "*.cpp","*.h","*.mk","*.target.mk","*.gyp" -Recurse | Remove-Item -Force -Verbose
}

# -------------------------------------------------------
# 5. Remove test/example/docs folders
# -------------------------------------------------------
Write-Host "[5] Removing test/example/docs folders..." -ForegroundColor Yellow
$foldersToRemove = @("test", "example", "docs", "examples")
foreach ($folder in $foldersToRemove) {
    if (Test-Path $codeServerPath) {
        Get-ChildItem -Path $codeServerPath -Directory -Include $folder -Recurse | Remove-Item -Recurse -Force -Verbose
    }
}

# -------------------------------------------------------
# 6. SKIPPED: npm prune --production
# -------------------------------------------------------
# This step was REMOVED because it deletes code-server itself!
# code-server is installed with --no-save, so npm considers it an
# "extraneous" package and removes it during prune --production.
# This caused v1.0.5-v1.0.9 to ship with an empty code-server dir.
#
# The other pruning steps (1-5, 7-9) are safe and remove ~180MB of
# non-Windows binaries, source maps, C++ source, test folders, etc.
# That brings the installer from ~200MB to ~23MB without breaking
# code-server.

# -------------------------------------------------------
# 7. Remove non-Windows native .node files
# -------------------------------------------------------
Write-Host "[7] Removing non-Windows .node binaries..." -ForegroundColor Yellow
if (Test-Path "$codeServerPath/node_modules") {
    Get-ChildItem -Path "$codeServerPath/node_modules" -Include "*.node" -Recurse | Where-Object {
        $_.FullName -notmatch "win32|windows|x64"
    } | Remove-Item -Force -Verbose
}

# -------------------------------------------------------
# 8. Remove any .log, .cache, .tmp files
# -------------------------------------------------------
Write-Host "[8] Removing log/cache/temp files..." -ForegroundColor Yellow
if (Test-Path $codeServerPath) {
    Get-ChildItem -Path $codeServerPath -Include "*.log","*.cache","*.tmp" -Recurse | Remove-Item -Force -Verbose
}

# -------------------------------------------------------
# 9. Disable Tauri devtools feature in Cargo.toml
# -------------------------------------------------------
Write-Host "[9] Disabling Tauri devtools feature..." -ForegroundColor Yellow
$cargoToml = "src-tauri/Cargo.toml"
if (Test-Path $cargoToml) {
    $content = Get-Content $cargoToml -Raw
    $content = $content -replace 'features\s*=\s*\[[^\]]*"devtools"[^\]]*\]', 'features = []'
    $content | Set-Content $cargoToml
    Write-Host "Cargo.toml updated."
}

Write-Host "[Prune] Optimization complete!" -ForegroundColor Green
