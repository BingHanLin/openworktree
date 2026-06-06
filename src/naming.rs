//! Random, human-readable worktree names (adjective-noun).

use rand::seq::SliceRandom;

const ADJECTIVES: &[&str] = &[
    "brave", "calm", "clever", "eager", "gentle", "happy", "jolly", "keen",
    "lively", "merry", "nimble", "proud", "quiet", "swift", "witty", "bold",
    "bright", "crisp", "fuzzy", "lucky", "mellow", "snug", "spry", "zesty",
];

const NOUNS: &[&str] = &[
    "otter", "finch", "lynx", "heron", "marten", "raven", "sparrow", "badger",
    "falcon", "ferret", "gecko", "ibex", "koala", "lemur", "magpie", "newt",
    "osprey", "puffin", "quokka", "robin", "shrew", "tapir", "vole", "wombat",
];

/// Pick a random `adjective-noun` pair, e.g. `brave-otter`.
pub fn random_name() -> String {
    let mut rng = rand::thread_rng();
    let adj = ADJECTIVES.choose(&mut rng).unwrap();
    let noun = NOUNS.choose(&mut rng).unwrap();
    format!("{adj}-{noun}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_adjective_dash_noun() {
        let name = random_name();
        let parts: Vec<&str> = name.split('-').collect();
        assert_eq!(parts.len(), 2, "expected adjective-noun, got {name}");
        assert!(ADJECTIVES.contains(&parts[0]));
        assert!(NOUNS.contains(&parts[1]));
    }
}
