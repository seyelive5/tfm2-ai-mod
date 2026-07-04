# Shim → build_mod.ps1 -CheckOnly. Typechecks the tfm2_patch_probe COMPILE CANARY
# (client-direct PatchNote read path) against the current SDK. Produces no DLL.
# Run after every game/SDK patch: green = authored_chg's route survives; red = the
# game restructured the news/patch types and authored_chg needs updating.
& "$PSScriptRoot\build_mod.ps1" -Crate (Join-Path $PSScriptRoot "tfm2_patch_probe") -CheckOnly @args
