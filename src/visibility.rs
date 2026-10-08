//! Account-local sidebar visibility; owner rules also cover newly discovered repositories.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hidden {
    #[serde(default)]
    pub repositories: BTreeSet<String>,
    #[serde(default)]
    pub owners: BTreeSet<String>,
}
impl Hidden {
    pub fn repository(&self, name: &str) -> bool {
        self.repositories.contains(&name.to_ascii_lowercase())
    }
    pub fn owner(&self, name: &str) -> bool {
        self.owners.contains(&name.to_ascii_lowercase())
    }
    pub fn contains(&self, name: &str, owner: &str) -> bool {
        self.repository(name) || self.owner(owner)
    }
    pub fn toggle_repository(&mut self, name: &str) {
        toggle(&mut self.repositories, name);
    }
    pub fn toggle_owner(&mut self, name: &str) {
        toggle(&mut self.owners, name);
    }
}
fn toggle(set: &mut BTreeSet<String>, name: &str) {
    let name = name.to_ascii_lowercase();
    if !set.remove(&name) {
        set.insert(name);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owner_rules_cover_new_repositories_and_restore_independently() {
        let mut hidden = Hidden::default();
        hidden.toggle_repository("Fixture/One");
        hidden.toggle_owner("FIXTURE");
        assert!(hidden.contains("fixture/future-repository", "fixture"));
        assert!(!hidden.contains("another/repo", "another"));
        hidden.toggle_repository("fixture/one");
        assert!(hidden.contains("fixture/one", "fixture"));
        hidden.toggle_repository("fixture/two");
        hidden.toggle_owner("fixture");
        assert!(!hidden.contains("fixture/one", "fixture"));
        assert!(hidden.contains("fixture/two", "fixture"));
        assert_eq!(
            serde_json::from_str::<Hidden>(&serde_json::to_string(&hidden).unwrap()).unwrap(),
            hidden
        );
    }
}
