use crate::{
    error::Error,
    manifest::{LintLevel, ProfileDef},
};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub dir: String,
    pub opt_level: u8,
    pub lints: BTreeMap<String, LintLevel>,
}

impl Profile {
    pub fn resolve(name: &str, defs: &BTreeMap<String, ProfileDef>) -> Result<Profile, Error> {
        let (opt_level, lints) = resolve_inner(name, defs, &mut Vec::new())?;
        let dir = match name {
            "dev" => "debug",
            other => other,
        };
        Ok(Profile {
            name: name.to_string(),
            dir: dir.to_string(),
            opt_level,
            lints,
        })
    }
}

fn builtin_opt_level(name: &str) -> Option<u8> {
    match name {
        "dev" => Some(0),
        "release" => Some(2),
        _ => None,
    }
}

fn resolve_inner(
    name: &str,
    defs: &BTreeMap<String, ProfileDef>,
    chain: &mut Vec<String>,
) -> Result<(u8, BTreeMap<String, LintLevel>), Error> {
    if chain.iter().any(|seen| seen == name) {
        return Err(Error::from(format!(
            "profile '{}' inherits from itself: {}",
            name,
            chain.join(" -> ")
        )));
    }
    let def = defs.get(name);
    let builtin = builtin_opt_level(name);
    if def.is_none() && builtin.is_none() {
        return Err(Error::with_help(
            format!("unknown profile '{}'", name),
            format!(
                "define [profile.{}] in the workspace Phage.toml, or use dev or release",
                name
            ),
        ));
    }
    let parent = def
        .and_then(|def| def.inherits.clone())
        .or_else(|| builtin.is_none().then(|| "dev".to_string()));

    chain.push(name.to_string());
    let (mut opt_level, mut lints) = match &parent {
        Some(parent) => resolve_inner(parent, defs, chain)?,
        None => (builtin.unwrap_or(0), BTreeMap::new()),
    };
    chain.pop();

    if let Some(def) = def {
        if let Some(level) = def.opt_level {
            opt_level = level;
        }
        lints.extend(def.lints.iter().map(|(name, level)| (name.clone(), *level)));
    }
    Ok((opt_level, lints))
}

pub fn overlay(
    base: &BTreeMap<String, LintLevel>,
    top: &BTreeMap<String, LintLevel>,
) -> BTreeMap<String, LintLevel> {
    let mut merged = base.clone();
    merged.extend(top.iter().map(|(name, level)| (name.clone(), *level)));
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defs(text: &str) -> BTreeMap<String, ProfileDef> {
        let manifest: crate::manifest::Manifest = toml::from_str(&format!(
            "[project]\nname = \"p\"\nversion = \"0.1.0\"\n{text}"
        ))
        .unwrap();
        manifest.profile
    }

    #[test]
    fn builtin_dev_and_release_levels() {
        let none = BTreeMap::new();
        let dev = Profile::resolve("dev", &none).unwrap();
        let release = Profile::resolve("release", &none).unwrap();
        assert_eq!((dev.opt_level, dev.dir.as_str()), (0, "debug"));
        assert_eq!((release.opt_level, release.dir.as_str()), (2, "release"));
    }

    #[test]
    fn custom_profile_inherits_and_overrides_opt_level() {
        let table = defs("[profile.fast]\ninherits = \"release\"\nopt-level = 3\n");
        let fast = Profile::resolve("fast", &table).unwrap();
        assert_eq!(fast.opt_level, 3);
        assert_eq!(fast.dir, "fast");
    }

    #[test]
    fn custom_profile_without_inherits_starts_from_dev() {
        let table = defs("[profile.scratch]\n");
        assert_eq!(Profile::resolve("scratch", &table).unwrap().opt_level, 0);
    }

    #[test]
    fn profile_lints_are_inherited_and_overridden() {
        let table = defs(
            "[profile.dev]\nlints = { never_mutated = \"warn\", old = \"allow\" }\n\n[profile.strict]\ninherits = \"dev\"\nlints = { never_mutated = \"deny\" }\n",
        );
        let strict = Profile::resolve("strict", &table).unwrap();
        assert_eq!(strict.lints.get("never_mutated"), Some(&LintLevel::Deny));
        assert_eq!(strict.lints.get("old"), Some(&LintLevel::Allow));
    }

    #[test]
    fn inheritance_cycle_is_an_error() {
        let table = defs("[profile.a]\ninherits = \"b\"\n\n[profile.b]\ninherits = \"a\"\n");
        let err = Profile::resolve("a", &table).unwrap_err();
        assert!(err.contains("inherits from itself"), "{err}");
    }

    #[test]
    fn unknown_profile_names_the_fix() {
        let err = Profile::resolve("nightly", &BTreeMap::new()).unwrap_err();
        assert!(err.contains("unknown profile 'nightly'"), "{err}");
        assert!(err.help.as_deref().unwrap().contains("[profile.nightly]"));
    }

    #[test]
    fn overlay_prefers_the_top_level() {
        let base = BTreeMap::from([("a".to_string(), LintLevel::Warn)]);
        let top = BTreeMap::from([("a".to_string(), LintLevel::Deny)]);
        assert_eq!(overlay(&base, &top).get("a"), Some(&LintLevel::Deny));
    }
}
