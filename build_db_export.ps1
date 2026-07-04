# Shim → build_mod.ps1 (unified builder). Kept because tfm2_db_export/src/lib.rs:15
# names this script. Forwards any extra args (e.g. -Sdk, -CheckOnly).
& "$PSScriptRoot\build_mod.ps1" -Crate (Join-Path $PSScriptRoot "tfm2_db_export") @args
