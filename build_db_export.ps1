# Shortcut → build_mod.ps1 for the recommender mod. Forwards any extra args
# (e.g. -Deploy, -CheckOnly, -GameDir).
& "$PSScriptRoot\build_mod.ps1" -Crate (Join-Path $PSScriptRoot "tfm2_db_export") @args
