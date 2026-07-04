# Unified builder for the tfm2 mods that link the game_view / engine_ui / game_core
# RLIB externs (tfm2_db_export, tfm2_patch_probe) on top of mod_api.
#
# The SDK's deps\ accumulates OLD+NEW rlib generations across game patches, so we
# pick a CONSISTENT set by the SDK's own embedded rustc hash — and that hash + the
# toolchain are DERIVED from the SDK itself ($Sdk\toolchain_version.txt, cross-checked
# with rust-toolchain.toml), so a game patch needs NO manual pin edits. Override with
# -Toolchain / -RustcHash only in the rare case the SDK metadata is wrong.
#
#   build_mod.ps1 -Crate <cratedir> [-CheckOnly] [-Sdk <sdkdir>] [-Toolchain <ch>] [-RustcHash <hash>]
#
# -CheckOnly : typecheck only (--emit=metadata), no DLL produced/copied.
# else       : build the cdylib and copy target\release\<crate>.dll into the crate folder.
param(
    [Parameter(Mandatory = $true)][string]$Crate,
    [switch]$CheckOnly,
    [string]$Sdk = "C:\tmp\mod-sdk",
    [string]$Toolchain = "",
    [string]$RustcHash = ""
)
$ErrorActionPreference = "Stop"

# ── derive toolchain + rustc hash from the SDK (no hand-maintained pins).
$tvPath = Join-Path $Sdk "toolchain_version.txt"
if (-not (Test-Path $tvPath)) { throw "SDK toolchain_version.txt not found at $tvPath" }
$tv = Get-Content $tvPath -Raw          # e.g. "rustc 1.98.0-nightly (23a3312d9 2026-05-23)"
if ($tv -notmatch '\(([0-9a-f]+)\s+(\d{4}-\d{2}-\d{2})\)') { throw "cannot parse rustc hash/date from '$tv'" }
if (-not $RustcHash) { $RustcHash = $Matches[1] }
if (-not $Toolchain) {
    # channel = rustc date + 1 (SDK convention); prefer the SDK's own rust-toolchain.toml if present.
    $rt = Join-Path $Sdk "rust-toolchain.toml"
    if ((Test-Path $rt) -and ((Get-Content $rt -Raw) -match 'channel\s*=\s*"([^"]+)"')) {
        $Toolchain = $Matches[1]
    } else {
        $Toolchain = "nightly-" + ([datetime]$Matches[2]).AddDays(1).ToString("yyyy-MM-dd")
    }
}

# ── nicety: warn (don't block) if the toolchain isn't installed yet.
try {
    $tlist = (& "$env:USERPROFILE\.cargo\bin\rustup.exe" toolchain list 2>$null) -join "`n"
    if ($tlist -and ($tlist -notmatch [regex]::Escape($Toolchain))) {
        Write-Warning "toolchain $Toolchain not installed; if the build fails run: rustup toolchain install $Toolchain --profile minimal"
    }
} catch { }

$prevToolchain = $env:RUSTUP_TOOLCHAIN
$env:RUSTUP_TOOLCHAIN = $Toolchain
$deps = Join-Path $Sdk "deps"
function Pick($pat) {
    Get-ChildItem (Join-Path $deps $pat) | Where-Object {
        $f = $_
        # Surface (don't silently mask) a locked/unreadable rlib as a warning, but keep
        # scanning the other candidates — only a genuine no-match throws below.
        try { Select-String -Path $f.FullName -Pattern $RustcHash -Quiet -ErrorAction Stop }
        catch { Write-Warning "skip unreadable rlib $($f.Name): $($_.Exception.Message)"; $false }
    } | Select-Object -First 1 -ExpandProperty FullName
}
try {
    $modApi   = Pick "libmod_api-*.rlib"
    $gameView = Pick "libgame_view-*.rlib"
    $engineUi = Pick "libengine_ui-*.rlib"
    $gameCore = Pick "libgame_core-*.rlib"
    if (-not ($modApi -and $gameView -and $engineUi -and $gameCore)) {
        throw "no rlib set matching rustc $RustcHash in $deps (SDK/toolchain mismatch — check $tvPath)"
    }
    Write-Host "SDK $Sdk  |  rustc $RustcHash  |  toolchain $Toolchain"
    Write-Host "  mod_api  = $(Split-Path $modApi -Leaf)"
    Write-Host "  game_view= $(Split-Path $gameView -Leaf)"
    Write-Host "  engine_ui= $(Split-Path $engineUi -Leaf)"
    Write-Host "  game_core= $(Split-Path $gameCore -Leaf)"

    $flags = @(
        "-L", "dependency=$deps",
        "--extern", "mod_api=$modApi",
        "--extern", "game_view=$gameView",
        "--extern", "engine_ui=$engineUi",
        "--extern", "game_core=$gameCore",
        "-L", "native=$(Join-Path $Sdk 'native')"
    )
    $env:CARGO_ENCODED_RUSTFLAGS = $flags -join [char]31

    $rustcArgs = @("--crate-type", "cdylib")
    if ($CheckOnly) { $rustcArgs += "--emit=metadata" }
    cargo rustc --release --manifest-path (Join-Path $Crate "Cargo.toml") `
        --target-dir (Join-Path $Crate "target") --lib -- @rustcArgs
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} finally {
    $env:CARGO_ENCODED_RUSTFLAGS = $null
    $env:RUSTUP_TOOLCHAIN = $prevToolchain
}

if ($CheckOnly) {
    Write-Host "Typecheck OK (no DLL): $Crate"
} else {
    $modId = (Split-Path $Crate -Leaf) -replace '-', '_'
    $dll = Join-Path $Crate "target\release\$modId.dll"
    Copy-Item $dll (Join-Path $Crate "$modId.dll") -Force
    Write-Host "Build successful: $(Join-Path $Crate "$modId.dll")"
}
