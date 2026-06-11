// existence test for candidate Scene variants (errors reveal which are absent)
pub fn a(v: &mod_api::Scene) { if let mod_api::Scene::Title { .. } = v {} }
pub fn b(v: &mod_api::Scene) { if let mod_api::Scene::Loading { .. } = v {} }
pub fn c(v: &mod_api::Scene) { if let mod_api::Scene::InGame { .. } = v {} }
pub fn d(v: &mod_api::Scene) { if let mod_api::Scene::Lobby { .. } = v {} }
pub fn e(v: &mod_api::Scene) { if let mod_api::Scene::ToTitle { .. } = v {} }
pub fn f(v: &mod_api::Scene) { if let mod_api::Scene::Main { .. } = v {} }
pub fn g(v: &mod_api::Scene) { if let mod_api::Scene::None { .. } = v {} }
pub fn h(v: &mod_api::Scene) { if let mod_api::Scene::Empty { .. } = v {} }
