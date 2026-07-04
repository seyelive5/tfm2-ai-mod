# Builds tfm2_db_export. Picks a CONSISTENT SDK rlib set by embedded rustc hash
# (deps now mixes old+new), links game_core too (for game_core::NewsType — the
# authored patch-notes route). Per patch: update $RustcHash + $Toolchain to match
# the new SDK's toolchain_version.txt (channel = rustc date + 1).
param(
    [string]$Sdk = "C:\tmp\mod-sdk",
    [string]$Toolchain = "nightly-2026-05-24",
    [string]$RustcHash = "23a3312d9",
    [string]$Crate = "C:\tmp\tfm2-ai-mod\tfm2_db_export"
)
$ErrorActionPreference = "Stop"
$prevToolchain = $env:RUSTUP_TOOLCHAIN
$env:RUSTUP_TOOLCHAIN = $Toolchain
$deps = Join-Path $Sdk "deps"
function Pick($pat) {
    Get-ChildItem (Join-Path $deps $pat) | Where-Object {
        Select-String -Path $_.FullName -Pattern $RustcHash -Quiet -ErrorAction SilentlyContinue
    } | Select-Object -First 1 -ExpandProperty FullName
}
$modApi   = Pick "libmod_api-*.rlib"
$gameView = Pick "libgame_view-*.rlib"
$engineUi = Pick "libengine_ui-*.rlib"
$gameCore = Pick "libgame_core-*.rlib"
if (-not ($modApi -and $gameView -and $engineUi -and $gameCore)) { throw "no rlib set matching rustc $RustcHash in $deps" }

$flags = @(
    "-L", "dependency=$deps",
    "--extern", "mod_api=$modApi",
    "--extern", "game_view=$gameView",
    "--extern", "engine_ui=$engineUi",
    "--extern", "game_core=$gameCore",
    "-L", "native=$(Join-Path $Sdk 'native')"
)
$env:CARGO_ENCODED_RUSTFLAGS = $flags -join [char]31
try {
    cargo rustc --release --manifest-path (Join-Path $Crate "Cargo.toml") `
        --target-dir (Join-Path $Crate "target") --lib -- --crate-type cdylib
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} finally {
    $env:CARGO_ENCODED_RUSTFLAGS = $null
    $env:RUSTUP_TOOLCHAIN = $prevToolchain
}
$dll = Join-Path $Crate "target\release\tfm2_db_export.dll"
Copy-Item $dll (Join-Path $Crate "tfm2_db_export.dll") -Force
Write-Host "Build successful: $(Join-Path $Crate 'tfm2_db_export.dll')"
