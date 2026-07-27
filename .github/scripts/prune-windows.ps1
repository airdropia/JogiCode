# ========================================================
# PRUNE-WINDOWS.PS1 — Windows Bundle Optimization (Aggressive)
# ========================================================
# Removes all non-runtime files from code-server to minimize
# installer size while keeping code-server fully functional.
# ========================================================
$ErrorActionPreference = "Continue"

Write-Host "[Prune] Starting aggressive Windows bundle optimization..." -ForegroundColor Cyan

$codeServerPath = "src-tauri/binaries/code-server"
$vscodePath = "$codeServerPath/node_modules/code-server/lib/vscode"

# -------------------------------------------------------
# 1. Remove non-Windows ripgrep binaries
# -------------------------------------------------------
Write-Host "[1] Removing non-Windows ripgrep binaries..." -ForegroundColor Yellow
$rgPath = "$codeServerPath/node_modules/vscode-ripgrep/bin"
if (Test-Path $rgPath) {
    Get-ChildItem -Path $rgPath -Recurse | Where-Object {
        $_.Name -notmatch "\.exe$" -and $_.Name -ne "rg.exe"
    } | Remove-Item -Force -Recurse -ErrorAction SilentlyContinue
}

# -------------------------------------------------------
# 2. Remove macOS icon
# -------------------------------------------------------
Write-Host "[2] Removing macOS icon..." -ForegroundColor Yellow
Remove-Item -Path "src-tauri/icons/icon.icns" -Force -ErrorAction SilentlyContinue

# -------------------------------------------------------
# 3. Remove source maps, TypeScript declarations, AND TypeScript source
# -------------------------------------------------------
Write-Host "[3] Removing source maps, .d.ts, and .ts files..." -ForegroundColor Yellow
if (Test-Path $codeServerPath) {
    # .map files (source maps)
    Get-ChildItem -Path $codeServerPath -Include "*.map" -Recurse -File | Remove-Item -Force -ErrorAction SilentlyContinue
    # .d.ts files (TypeScript declarations)
    Get-ChildItem -Path $codeServerPath -Include "*.d.ts" -Recurse -File | Remove-Item -Force -ErrorAction SilentlyContinue
    # .ts files (TypeScript source — NOT needed at runtime, only .js is needed)
    Get-ChildItem -Path $codeServerPath -Include "*.ts" -Recurse -File | Where-Object { $_.Name -notmatch "\.d\.ts$" } | Remove-Item -Force -ErrorAction SilentlyContinue
}

# -------------------------------------------------------
# 4. Remove C++ source artifacts
# -------------------------------------------------------
Write-Host "[4] Removing C++ source artifacts..." -ForegroundColor Yellow
if (Test-Path "$codeServerPath/node_modules") {
    Get-ChildItem -Path "$codeServerPath/node_modules" -Include "*.cpp","*.h","*.mk","*.target.mk","*.gyp","*.gypi","*.cc","*.hpp" -Recurse -File | Remove-Item -Force -ErrorAction SilentlyContinue
}

# -------------------------------------------------------
# 5. Remove test/example/docs/build folders
# -------------------------------------------------------
Write-Host "[5] Removing test/example/docs/build folders..." -ForegroundColor Yellow
$foldersToRemove = @("test", "tests", "test-resources", "example", "examples", "docs", "doc", "build", ".github", ".vscode", ".vscode-test", "coverage", ".nyc_output", "benchmarks", "bench", "scripts")
foreach ($folder in $foldersToRemove) {
    if (Test-Path $codeServerPath) {
        Get-ChildItem -Path $codeServerPath -Directory -Include $folder -Recurse | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# -------------------------------------------------------
# 6. Remove devDependencies (npm prune --production)
# -------------------------------------------------------
Write-Host "[6] Pruning devDependencies..." -ForegroundColor Yellow
if (Test-Path $codeServerPath) {
    Push-Location $codeServerPath
    npm prune --production 2>&1 | Write-Host
    Pop-Location
}

# Also prune devDependencies from VS Code's node_modules
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
    Get-ChildItem -Path "$codeServerPath/node_modules" -Include "*.node" -Recurse -File | Where-Object {
        $_.FullName -notmatch "win32|windows|x64"
    } | Remove-Item -Force -ErrorAction SilentlyContinue
}

# -------------------------------------------------------
# 8. Remove log/cache/temp files
# -------------------------------------------------------
Write-Host "[8] Removing log/cache/temp files..." -ForegroundColor Yellow
if (Test-Path $codeServerPath) {
    Get-ChildItem -Path $codeServerPath -Include "*.log","*.cache","*.tmp","*.bak","*.orig" -Recurse -File | Remove-Item -Force -ErrorAction SilentlyContinue
}

# -------------------------------------------------------
# 9. Remove documentation files (README, LICENSE, CHANGELOG)
# -------------------------------------------------------
Write-Host "[9] Removing documentation files..." -ForegroundColor Yellow
if (Test-Path $codeServerPath) {
    Get-ChildItem -Path $codeServerPath -Include "*.md","*.markdown","README*","LICENSE*","CHANGELOG*","HISTORY*","AUTHORS*","CONTRIBUTING*" -Recurse -File | Remove-Item -Force -ErrorAction SilentlyContinue
}

# -------------------------------------------------------
# 10. Remove .bin directories in node_modules (CLI symlinks, not needed at runtime)
# -------------------------------------------------------
Write-Host "[10] Removing .bin directories..." -ForegroundColor Yellow
if (Test-Path "$codeServerPath/node_modules") {
    Get-ChildItem -Path "$codeServerPath/node_modules" -Directory -Include ".bin" -Recurse | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
}

# -------------------------------------------------------
# 11. Remove lock files and npm metadata
# -------------------------------------------------------
Write-Host "[11] Removing lock files and npm metadata..." -ForegroundColor Yellow
if (Test-Path $codeServerPath) {
    Get-ChildItem -Path $codeServerPath -Include "package-lock.json","yarn.lock","*.tgz",".npmrc",".yarnrc" -Recurse -File | Remove-Item -Force -ErrorAction SilentlyContinue
}

# -------------------------------------------------------
# 12. Remove .cache directories
# -------------------------------------------------------
Write-Host "[12] Removing .cache directories..." -ForegroundColor Yellow
if (Test-Path $codeServerPath) {
    Get-ChildItem -Path $codeServerPath -Directory -Include ".cache",".parcel-cache","cache" -Recurse | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
}

# -------------------------------------------------------
# 13. Remove .git directories
# -------------------------------------------------------
Write-Host "[13] Removing .git directories..." -ForegroundColor Yellow
if (Test-Path $codeServerPath) {
    Get-ChildItem -Path $codeServerPath -Directory -Include ".git" -Recurse | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
}

# -------------------------------------------------------
# 14. Disable Tauri devtools feature in Cargo.toml
# -------------------------------------------------------
Write-Host "[14] Disabling Tauri devtools feature..." -ForegroundColor Yellow
$cargoToml = "src-tauri/Cargo.toml"
if (Test-Path $cargoToml) {
    $content = Get-Content $cargoToml -Raw
    $content = $content -replace 'features\s*=\s*\[[^\]]*"devtools"[^\]]*\]', 'features = []'
    $content | Set-Content $cargoToml
    Write-Host "Cargo.toml updated."
}

# -------------------------------------------------------
# 15. Report final size
# -------------------------------------------------------
Write-Host "[15] Reporting final size..." -ForegroundColor Yellow
if (Test-Path $codeServerPath) {
    $size = (Get-ChildItem -Path $codeServerPath -Recurse -File | Measure-Object -Property Length -Sum).Sum
    $sizeMB = [math]::Round($size / 1MB, 2)
    Write-Host "Final code-server size: $sizeMB MB" -ForegroundColor Green
}

Write-Host "[Prune] Aggressive optimization complete!" -ForegroundColor Green
