# Builds tfm2_db_export — which (since the bp_reader merge) needs the game_view +
# engine_ui RLIB externs on top of mod_api. Their filename hashes change EVERY game
# patch, so they are globbed from the SDK rather than hardcoded.
#
# Per-patch flow: install the new SDK at $Sdk → install the toolchain its
# toolchain_version.txt implies (channel ≈ rustc date + 1) → run this with -Toolchain.
param(
    [string]$Sdk = "C:\tmp\mod-sdk",
    [string]$Toolchain = "nightly-2026-06-08",
    [string]$Crate = "C:\tmp\tfm2-ai-mod\tfm2_db_export"
)
$ErrorActionPreference = "Stop"
$env:RUSTUP_TOOLCHAIN = $Toolchain

$deps = Join-Path $Sdk "deps"
$pick = { param($pat) (Get-ChildItem (Join-Path $deps $pat) | Select-Object -First 1).FullName }
$modApi   = & $pick "libmod_api-*.rlib"
$gameView = & $pick "libgame_view-*.rlib"
$engineUi = & $pick "libengine_ui-*.rlib"
if (-not ($modApi -and $gameView -and $engineUi)) { throw "SDK rlibs not found in $deps" }

$flags = @(
    "-L", "dependency=$deps",
    "--extern", "mod_api=$modApi",
    "--extern", "game_view=$gameView",
    "--extern", "engine_ui=$engineUi",
    "-L", "native=$(Join-Path $Sdk 'native')"
)
$env:CARGO_ENCODED_RUSTFLAGS = $flags -join [char]31
try {
    cargo rustc --release --manifest-path (Join-Path $Crate "Cargo.toml") `
        --target-dir (Join-Path $Crate "target") --lib -- --crate-type cdylib
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} finally {
    $env:CARGO_ENCODED_RUSTFLAGS = $null
}
$dll = Join-Path $Crate "target\release\tfm2_db_export.dll"
Copy-Item $dll (Join-Path $Crate "tfm2_db_export.dll") -Force
Write-Host "Build successful: $(Join-Path $Crate 'tfm2_db_export.dll')"
