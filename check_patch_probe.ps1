# Compile-only probe: typecheck tfm2_patch_probe against a CONSISTENT SDK rlib set
# (picks each rlib by embedded rustc hash, so mixed old+new deps don't break it) and
# print the compiler errors. Never produces/loads a DLL. No game required.
param(
    [string]$Sdk = "C:\tmp\mod-sdk",
    [string]$Toolchain = "nightly-2026-05-24",
    [string]$RustcHash = "23a3312d9",      # the rustc commit the current SDK rlibs were built with
    [string]$Crate = "C:\tmp\tfm2-ai-mod\tfm2_patch_probe"
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
Write-Host "Using rlibs (rustc $RustcHash):"
Write-Host "  mod_api  = $(Split-Path $modApi -Leaf)"
Write-Host "  game_core= $(Split-Path $gameCore -Leaf)"
Write-Host "  game_view= $(Split-Path $gameView -Leaf)"
Write-Host "  engine_ui= $(Split-Path $engineUi -Leaf)"

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
        --target-dir (Join-Path $Crate "target") --lib -- --crate-type cdylib --emit=metadata
} finally {
    $env:CARGO_ENCODED_RUSTFLAGS = $null
    $env:RUSTUP_TOOLCHAIN = $prevToolchain
}
