# Unified builder for TFM2 mods (Stable SDK v0.6.5+ / mod-api-stable).
#
#   build_mod.ps1 -Crate <cratedir> [-CheckOnly] [-Deploy]
#
# -CheckOnly : typecheck only (cargo check), no DLL copied.
# -Deploy    : also copy <modId>.dll + mod.mod_info into the Steam game's mods\<modId>\ directory.
param(
    [Parameter(Mandatory = $true)][string]$Crate,
    [switch]$CheckOnly,
    [switch]$Deploy = $true,
    [string]$GameDir = "C:\Program Files (x86)\Steam\steamapps\common\Teamfight Manager2"
)
$ErrorActionPreference = "Stop"

$manifest = Join-Path $Crate "Cargo.toml"
if (-not (Test-Path $manifest)) { throw "Cargo.toml not found at $manifest" }

if ($CheckOnly) {
    cargo check --release --manifest-path $manifest --target-dir (Join-Path $Crate "target")
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    Write-Host "Typecheck OK (no DLL): $Crate"
    exit 0
}

cargo build --release --manifest-path $manifest --target-dir (Join-Path $Crate "target")
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$modId = (Split-Path $Crate -Leaf) -replace '-', '_'
$dll = Join-Path $Crate "target\release\$modId.dll"
Copy-Item $dll (Join-Path $Crate "$modId.dll") -Force
Write-Host "Build successful: $(Join-Path $Crate "$modId.dll")"

if ($Deploy -and (Test-Path $GameDir)) {
    $destDir = Join-Path $GameDir "mods\$modId"
    New-Item -ItemType Directory -Force -Path $destDir | Out-Null
    Copy-Item $dll (Join-Path $destDir "$modId.dll") -Force
    $modInfo = Join-Path $Crate "mod.mod_info"
    if (Test-Path $modInfo) {
        Copy-Item $modInfo (Join-Path $destDir "mod.mod_info") -Force
    }
    Write-Host "Deployed to game: $destDir"
}
