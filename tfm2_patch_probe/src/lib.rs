//! COMPILE CANARY — NOT a loadable mod (no `declare_mod!`, no mod.mod_info).
//! Verifies the client-direct PatchNote read path that `tfm2_db_export::authored_chg`
//! depends on still typechecks against the current SDK. Run after every game patch:
//!   check_patch_probe.ps1   (→ build_mod.ps1 -CheckOnly, produces no DLL)
//! GREEN  = db.teams → Team.news → NewsType::PatchNote → ChampionPatchData{is_buff}
//!          is intact; authored_chg's route survives the new SDK.
//! RED    = the game restructured the news/patch types → authored_chg needs updating
//!          before the recommender's `chg` can trust the authored route again.
#![allow(unused, dead_code, unused_imports)]
use mod_api::*;
use game_core::NewsType;

// Mirror of authored_chg's exact read shape (tfm2_db_export/src/lib.rs). If any field
// or variant moved this stops compiling — that IS the canary firing.
fn client_read(db: &ClientDatabase) {
    for (_id, team) in db.teams.iter() {
        for news in &team.news {
            if let NewsType::PatchNote { version, champion_patch_data, .. } = &news.ty {
                let _ = version;
                for (champ, changes) in champion_patch_data {
                    let _ = champ;
                    for cpd in changes {
                        let _ = (&cpd.key, &cpd.is_buff);
                    }
                }
            }
        }
    }
}
