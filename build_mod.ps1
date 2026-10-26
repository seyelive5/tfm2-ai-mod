# Builder for TFM2 stable-ABI mods (game 0.6+, mod-api-stable).
#
#   build_mod.ps1 -Crate <cratedir> [-CheckOnly] [-Deploy] [-GameDir <path>]
#
# The SDK is the game's own `mod-sdk-stable` folder. Cargo.toml points at a copy in
# the repo's gitignored sdk\mod-sdk-stable; this script refreshes that copy from the
# game folder whenever the game's copy differs (base_version.txt + the contract's
# lib.rs hash) or the repo copy is missing. Any Rust toolchain builds it — only the
# repr(C) contract crosses the DLL boundary (SDK README) — so nothing is pinned.
#
# -CheckOnly : typecheck only (cargo check), no DLL.
# -Deploy    : also copy <modId>.dll + mod.mod_info into <GameDir>\mods\<modId>\.
param(
    [Parameter(Mandatory = $true)][string]$Crate,
    [switch]$CheckOnly,
    [switch]$Deploy,
    [string]$GameDir = "C:\Program Files (x86)\Steam\steamapps\common\Teamfight Manager2"
)
$ErrorActionPreference = "Stop"

$manifest = Join-Path $Crate "Cargo.toml"
if (-not (Test-Path $manifest)) { throw "Cargo.toml not found at $manifest" }

function Get-SdkStamp([string]$dir) {
    $ver = Join-Path $dir "base_version.txt"
    $lib = Join-Path $dir "mod-api-stable\src\lib.rs"
    if (-not ((Test-Path $ver) -and (Test-Path $lib))) { return $null }
    return "$((Get-Content $ver -Raw).Trim()) / $((Get-FileHash $lib -Algorithm SHA256).Hash.Substring(0, 12))"
}

$sdkSrc = Join-Path $GameDir "mod-sdk-stable"
$sdkDst = Join-Path $PSScriptRoot "sdk\mod-sdk-stable"
$srcStamp = Get-SdkStamp $sdkSrc
$dstStamp = Get-SdkStamp $sdkDst
if ($srcStamp -and $srcStamp -ne $dstStamp) {
    Write-Host "SDK: refreshing sdk\mod-sdk-stable from the game folder ($dstStamp -> $srcStamp)"
    if (Test-Path $sdkDst) { Remove-Item -Recurse -Force $sdkDst }
    New-Item -ItemType Directory -Force -Path (Split-Path $sdkDst) | Out-Null
    Copy-Item -Recurse $sdkSrc $sdkDst
} elseif (-not $dstStamp) {
    throw "No SDK: $sdkDst is missing and $sdkSrc has no mod-sdk-stable (pass -GameDir)"
} else {
    Write-Host "SDK: $dstStamp"
}

if ($CheckOnly) {
    cargo check --release --manifest-path $manifest
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    Write-Host "Typecheck OK (no DLL): $Crate"
    exit 0
}

cargo build --release --manifest-path $manifest
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$modId = (Split-Path $Crate -Leaf) -replace '-', '_'
$dll = Join-Path $Crate "target\release\$modId.dll"
Copy-Item $dll (Join-Path $Crate "$modId.dll") -Force
Write-Host "Build successful: $(Join-Path $Crate "$modId.dll")"

if ($Deploy) {
    if (-not (Test-Path $GameDir)) { throw "GameDir not found: $GameDir" }
    $destDir = Join-Path $GameDir "mods\$modId"
    New-Item -ItemType Directory -Force -Path $destDir | Out-Null
    Copy-Item $dll (Join-Path $destDir "$modId.dll") -Force
    $modInfo = Join-Path $Crate "mod.mod_info"
    if (Test-Path $modInfo) {
        Copy-Item $modInfo (Join-Path $destDir "mod.mod_info") -Force
    }
    Write-Host "Deployed to game: $destDir"
}
