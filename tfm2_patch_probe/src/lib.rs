//! COMPILE-ONLY probe, round 8: can the CLIENT read PatchNote directly?
//! If ClientDatabase has `teams` (→ Team.news → NewsType::PatchNote), the whole
//! server-ext + mod_save bridge is unnecessary — tfm2_db_export's existing client
//! code can build `chg` straight from the latest PatchNote.
#![allow(unused, dead_code, unused_imports)]
use mod_api::*;
use game_core::NewsType;

fn client_direct(db: &ClientDatabase) {
    let _: () = &db.teams; // E0308 = present (reveals type) / E0609 = absent
}

// if teams exists, confirm the full client-side read path compiles:
fn client_read(db: &ClientDatabase) {
    for team in db.teams.iter() {
        for news in &team.news {
            if let NewsType::PatchNote { version, champion_patch_data, .. } = &news.ty {
                let _ = version;
                for (champ, changes) in champion_patch_data {
                    for cpd in changes {
                        let _ = (&cpd.key, &cpd.is_buff);
                    }
                }
            }
        }
    }
}
