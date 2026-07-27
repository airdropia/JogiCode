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
# 6. Remove devDependencies (npm prune --production)
# -------------------------------------------------------
# This is SAFE now because code-server was installed with --save,
# so it's in package.json dependencies. npm prune --production only
# removes devDependencies (test/build tools), NOT code-server itself.
# This removes ~150MB of devDependencies.
Write-Host "[6] Pruning devDependencies..." -ForegroundColor Yellow
if (Test-Path $codeServerPath) {
    Push-Location $codeServerPath
    npm prune --production 2>&1 | Write-Host
    Pop-Location
}

# Also prune devDependencies from VS Code's node_modules
$vscodePath = "$codeServerPath/node_modules/code-server/lib/vscode"
if (Test-Path $vscodePath) {
    Write-Host "[6b] Pruning VS Code devDependencies..." -ForegroundColor Yellow
    Push-Location $vscodePath
    npm prune --production 2>&1 | Write-Host
    Pop-Location
}

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
