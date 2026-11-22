//! Moving Milestone 1's context-aware dictation settings into the provider
//! registry, with no action from the owner and no key lost.
//!
//! A role (`[context.scene]`, `[context.edit]`) with its own `endpoint` and
//! no `provider` becomes a role that names a provider:
//!
//! 1. The endpoint is matched to a built-in kind when it plainly is one
//!    (`openrouter.ai`, `api.openai.com`, `api.anthropic.com`), else a
//!    `custom` provider with that base URL. An entry already in
//!    `[providers]` for the same place is reused; two roles on one place
//!    share one provider.
//! 2. The role's key (`context-keys.toml`'s store) is written to the
//!    provider's item and **read back**; a provider that already has a
//!    different key is a conflict, and that role is left as it was, working
//!    on its own endpoint, and named in the report.
//! 3. `config.toml` gets `[providers.<id>]`, the role's `provider`, and loses
//!    the role's `endpoint`, in one validated write; every other byte stays
//!    (`config::edit`).
//! 4. Only then is the role's old key item removed.
//!
//! Run again, it finds nothing to do. A key that cannot be read leaves its
//! role as it was. The cloud voice's key, which sat in `config.toml` on
//! Linux, is moved by the voice's own start-up path into the provider's item
//! (`secrets::VoiceKey::adopt` over `keys::VoiceProviderKey`); [`plan`]
//! reports it.
use super::keys::ProviderKeys;
use super::Kind;
use crate::config::{Config, Paths, ProviderEntry};
use crate::context::keys::{ContextKeys, Role};
use std::collections::BTreeMap;

/// `endpoint` as a base URL: no `/chat/completions` (Milestone 1 took
/// either), no trailing slash.
#[must_use]
pub fn base_of(endpoint: &str) -> String {
    let trimmed = endpoint.trim().trim_end_matches('/');
    trimmed
        .strip_suffix("/chat/completions")
        .unwrap_or(trimmed)
        .trim_end_matches('/')
        .to_string()
}

/// The built-in kind a base URL plainly is, else `custom`.
#[must_use]
pub fn kind_of(base_url: &str) -> Kind {
    let host = crate::grokbot::webhook::host(base_url).to_ascii_lowercase();
    let host = host.rsplit_once(':').map_or(host.as_str(), |(name, _)| name).to_string();
    match host.as_str() {
        "openrouter.ai" => Kind::OpenRouter,
        "api.openai.com" => Kind::OpenAi,
        "api.anthropic.com" => Kind::Anthropic,
        _ => Kind::Custom,
    }
}

/// One role's move, as planned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleMove {
    pub role: Role,
    /// The provider it will name.
    pub provider: String,
    /// The provider is new: this entry is added.
    pub added: Option<ProviderEntry>,
}

/// What the migration found and would do (or did).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub moves: Vec<RoleMove>,
    /// Roles left on their own endpoint, and why (no key is named).
    pub left: Vec<(Role, String)>,
    /// Keys carried into a provider's item (`edit → openrouter`).
    pub keys_carried: Vec<(Role, String)>,
    /// Old role key items removed after the move.
    pub old_keys_removed: Vec<Role>,
    /// `config.toml` still has the cloud voice's key in clear (the voice's
    /// start-up moves it).
    pub voice_key_in_file: bool,
    /// `config.toml` was written.
    pub wrote: bool,
}

/// What would move, from `config` alone: no store is asked.
#[must_use]
pub fn plan(config: &Config) -> Report {
    let mut report = Report {
        voice_key_in_file: config.voice.tts.has_key(),
        ..Report::default()
    };
    // Entries as they will be after the moves, so two roles on one place
    // share a new provider.
    let mut entries: BTreeMap<String, ProviderEntry> = config.providers.clone();
    for role in Role::ALL {
        let model = match role {
            Role::Scene => &config.context.scene,
            Role::Edit => &config.context.edit,
        };
        if !model.provider.trim().is_empty() || model.endpoint.trim().is_empty() {
            continue;
        }
        let base = base_of(&model.endpoint);
        if crate::context::http::url_problem(&base).is_some() {
            report
                .left
                .push((role, "its endpoint is not an http:// or https:// URL".into()));
            continue;
        }
        let kind = kind_of(&base);
        let wanted = |entry: &ProviderEntry| {
            Kind::parse(&entry.kind) == Some(kind)
                && match kind {
                    Kind::Custom => entry.base_url.trim().trim_end_matches('/') == base,
                    _ => entry.base_url.trim().is_empty()
                        || entry.base_url.trim().trim_end_matches('/') == base,
                }
        };
        let (provider, added) = match entries.iter().find(|(_, entry)| wanted(entry)) {
            Some((id, _)) => (id.clone(), None),
            None => {
                let stem = kind.word();
                let id = (1..)
                    .map(|n| if n == 1 { stem.to_string() } else { format!("{stem}-{n}") })
                    .find(|id| !entries.contains_key(id))
                    .unwrap_or_else(|| stem.to_string());
                let entry = ProviderEntry {
                    kind: kind.word().into(),
                    label: None,
                    base_url: if kind == Kind::Custom { base.clone() } else { String::new() },
                    timeout_ms: None,
                };
                entries.insert(id.clone(), entry.clone());
                (id, Some(entry))
            }
        };
        report.moves.push(RoleMove {
            role,
            provider,
            added,
        });
    }
    report
}

/// Carry out [`plan`] on the service's `config.toml` and key stores.
/// `dry_run` reads the stores (to find conflicts) and writes nothing.
/// Returns the report and, when the file was written, the config now on
/// disk.
pub fn migrate(
    paths: &Paths,
    old_keys: &ContextKeys,
    provider_keys: &ProviderKeys,
    dry_run: bool,
) -> Result<(Report, Option<Config>), String> {
    let text = match std::fs::read_to_string(&paths.config) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Report::default(), None)),
        Err(e) => return Err(format!("{}: {e}", paths.config.display())),
    };
    let config = Config::parse(&text).map_err(|e| e.to_string())?;
    let mut report = plan(&config);
    // The keys first: a role whose key cannot be carried is not moved.
    let mut moves = Vec::new();
    let mut carry: Vec<(Role, String, String)> = Vec::new();
    // Every moved role that had a key of its own: its old item goes after
    // the write, whether its key was carried or the provider had it already.
    let mut had_key: Vec<Role> = Vec::new();
    for planned in std::mem::take(&mut report.moves) {
        let old = match old_keys.get(planned.role) {
            Ok(old) => old,
            Err(why) => {
                report.left.push((planned.role, format!("its key could not be read ({why})")));
                continue;
            }
        };
        if let Some(old) = old {
            let current = match provider_keys.get(&planned.provider) {
                Ok(current) => current,
                Err(why) => {
                    report.left.push((planned.role, format!("the provider's key could not be read ({why})")));
                    continue;
                }
            };
            // A key this run already carries for the same provider.
            let carried = carry
                .iter()
                .find(|(_, id, _)| *id == planned.provider)
                .map(|(_, _, key)| key.clone());
            match current.or(carried) {
                Some(theirs) if theirs != old => {
                    report.left.push((
                        planned.role,
                        format!("provider {} already has another key", planned.provider),
                    ));
                    continue;
                }
                Some(_) => {}
                None => carry.push((planned.role, planned.provider.clone(), old)),
            }
            had_key.push(planned.role);
        }
        moves.push(planned);
    }
    report.moves = moves;
    if dry_run || report.moves.is_empty() {
        report.keys_carried = carry.iter().map(|(role, id, _)| (*role, id.clone())).collect();
        return Ok((report, None));
    }
    for (role, id, key) in &carry {
        provider_keys
            .set(id, key)
            .map_err(|why| format!("provider {id} did not take {}'s key: {why}", role.name()))?;
        match provider_keys.get(id) {
            Ok(Some(back)) if back == *key => report.keys_carried.push((*role, id.clone())),
            _ => return Err(format!("provider {id}'s key did not read back; nothing else was changed")),
        }
    }
    // One validated write of the file.
    let mut edited = text;
    for planned in &report.moves {
        if let Some(entry) = &planned.added {
            let base = format!("providers.{}", planned.provider);
            edited = crate::config::edit(&edited, &format!("{base}.kind"), &entry.kind.clone().into())
                .map_err(|e| e.to_string())?;
            if !entry.base_url.is_empty() {
                edited = crate::config::edit(&edited, &format!("{base}.base_url"), &entry.base_url.clone().into())
                    .map_err(|e| e.to_string())?;
            }
        }
        let role = format!("context.{}", planned.role.name());
        edited = crate::config::edit(&edited, &format!("{role}.provider"), &planned.provider.clone().into())
            .map_err(|e| e.to_string())?;
        edited = crate::config::edit(&edited, &format!("{role}.endpoint"), &serde_json::Value::Null)
            .map_err(|e| e.to_string())?;
    }
    let next = Config::parse(&edited).map_err(|e| format!("the migrated config would not load: {e}"))?;
    crate::config::write(&paths.config, &edited).map_err(|e| e.to_string())?;
    report.wrote = true;
    // Only now: the old role key items.
    for role in &had_key {
        match old_keys.clear(*role) {
            Ok(_) => report.old_keys_removed.push(*role),
            Err(why) => tracing::warn!(
                role = role.name(),
                "providers: the old key item stays beside the provider's ({why})"
            ),
        }
    }
    Ok((report, Some(next)))
}

/// The migration at the service's start, on the main thread before the
/// core: the config now on disk when it wrote one. Logged by role and
/// provider, never a key or a URL.
pub fn at_start(paths: &Paths) -> Option<Config> {
    let old = ContextKeys::home(paths);
    let keys = ProviderKeys::home(paths);
    match migrate(paths, &old, &keys, false) {
        Ok((report, next)) => {
            for planned in &report.moves {
                tracing::info!(
                    role = planned.role.name(),
                    provider = %planned.provider,
                    added = planned.added.is_some(),
                    "providers: a context-aware dictation role moved into the registry"
                );
            }
            for (role, why) in &report.left {
                tracing::warn!(role = role.name(), "providers: left on its own endpoint: {why}");
            }
            next
        }
        Err(why) => {
            tracing::warn!("providers: the migration did not run: {why}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::keys::{MemoryStore, SecretStore as _, Vault};
    use crate::integrations::testdir::TestDir;
    use std::sync::Arc;

    const EDIT_KEY: &str = "sk-or-v1-edit-0123456789-WXYZ";
    const SCENE_KEY: &str = "sk-local-scene-0123-ABCD";

    struct Rig {
        _dir: TestDir,
        paths: Paths,
        store: Arc<MemoryStore>,
        old: ContextKeys,
        keys: ProviderKeys,
    }

    fn rig(name: &str, config: &str) -> Rig {
        let dir = TestDir::new(name);
        let data = dir.path().to_path_buf();
        let paths = Paths::from_data_dir(data.clone(), data.join("logs"), data.join("caches"));
        std::fs::write(&paths.config, config).unwrap();
        let store = Arc::new(MemoryStore::default());
        let old = ContextKeys::with(&data, Some(store.clone()));
        let keys = ProviderKeys::with(Vault::with(&data.join(super::super::keys::FILE), Some(store.clone())));
        Rig { _dir: dir, paths, store, old, keys }
    }

    /// The shape the owner's machine has (values stand-ins).
    const MILESTONE_ONE: &str = r#"# my own comment
[voice.stt]
engine = "whisper"

[context]
enabled = true

[context.edit]
endpoint = "https://openrouter.ai/api/v1"
model = "deepseek/deepseek-v4.1-flash-1017"

[context.scene]
endpoint = "http://192.168.1.10:11434/v1/chat/completions"
model = "qwen2.5:3b"
"#;

    #[test]
    fn roles_move_into_the_registry_and_their_keys_are_carried_then_the_old_ones_removed() {
        let r = rig("migrate-roles", MILESTONE_ONE);
        r.old.set(Role::Edit, EDIT_KEY).unwrap();
        r.old.set(Role::Scene, SCENE_KEY).unwrap();
        let (report, next) = migrate(&r.paths, &r.old, &r.keys, false).unwrap();
        let next = next.expect("written");
        assert!(report.wrote && report.left.is_empty(), "{report:?}");
        assert_eq!(next.context.edit.provider, "openrouter");
        assert_eq!(next.context.edit.model, "deepseek/deepseek-v4.1-flash-1017");
        assert_eq!(next.context.scene.provider, "custom");
        assert_eq!(next.providers["openrouter"], ProviderEntry { kind: "openrouter".into(), ..Default::default() });
        assert_eq!(next.providers["custom"].base_url, "http://192.168.1.10:11434/v1");
        assert!(next.context.edit.endpoint.is_empty() && next.context.scene.endpoint.is_empty());
        let text = std::fs::read_to_string(&r.paths.config).unwrap();
        assert!(text.starts_with("# my own comment\n[voice.stt]\nengine = \"whisper\"\n"), "{text}");
        assert!(!text.contains("endpoint"), "{text}");
        // Keys carried, read back, and only then the old items gone.
        assert_eq!(r.keys.get("openrouter").unwrap().as_deref(), Some(EDIT_KEY));
        assert_eq!(r.keys.get("custom").unwrap().as_deref(), Some(SCENE_KEY));
        assert_eq!(r.store.get(&Role::Edit.account()).unwrap(), None);
        assert_eq!(r.store.get(&Role::Scene.account()).unwrap(), None);
        assert_eq!(report.old_keys_removed, vec![Role::Scene, Role::Edit]);
        // The roles resolve to the same places as before.
        let edit = next.context_target(Role::Edit).unwrap();
        assert_eq!((edit.kind, edit.base_url.as_str()), (Kind::OpenRouter, "https://openrouter.ai/api/v1"));
        // Idempotent: a second run finds nothing and writes nothing.
        let before = std::fs::read_to_string(&r.paths.config).unwrap();
        let (again, next) = migrate(&r.paths, &r.old, &r.keys, false).unwrap();
        assert!(again.moves.is_empty() && next.is_none() && !again.wrote);
        assert_eq!(std::fs::read_to_string(&r.paths.config).unwrap(), before);
        assert_eq!(r.keys.get("openrouter").unwrap().as_deref(), Some(EDIT_KEY));
    }

    #[test]
    fn two_roles_on_one_place_share_one_provider_and_an_existing_entry_is_reused() {
        let r = rig(
            "migrate-share",
            "[providers.mine]\nkind = \"openrouter\"\n[context.edit]\nendpoint = \"https://openrouter.ai/api/v1/\"\nmodel = \"a\"\n[context.scene]\nendpoint = \"https://openrouter.ai/api/v1\"\nmodel = \"b\"\n",
        );
        r.old.set(Role::Edit, EDIT_KEY).unwrap();
        r.old.set(Role::Scene, EDIT_KEY).unwrap();
        let (report, next) = migrate(&r.paths, &r.old, &r.keys, false).unwrap();
        let next = next.unwrap();
        assert_eq!((next.context.edit.provider.as_str(), next.context.scene.provider.as_str()), ("mine", "mine"));
        assert_eq!(next.providers.len(), 1);
        assert!(report.left.is_empty());
        assert_eq!(r.keys.get("mine").unwrap().as_deref(), Some(EDIT_KEY));
        assert_eq!(report.old_keys_removed, vec![Role::Scene, Role::Edit], "both old items go");
        assert_eq!(r.store.get(&Role::Edit.account()).unwrap(), None);
    }

    #[test]
    fn a_conflicting_key_leaves_the_role_working_as_it_was() {
        let r = rig("migrate-conflict", "[providers.openrouter]\nkind = \"openrouter\"\n[context.edit]\nendpoint = \"https://openrouter.ai/api/v1\"\nmodel = \"m\"\n");
        r.old.set(Role::Edit, EDIT_KEY).unwrap();
        r.keys.set("openrouter", "sk-someone-elses-key-0000").unwrap();
        let before = std::fs::read_to_string(&r.paths.config).unwrap();
        let (report, next) = migrate(&r.paths, &r.old, &r.keys, false).unwrap();
        assert!(next.is_none() && report.moves.is_empty());
        assert_eq!(report.left.len(), 1);
        assert!(!format!("{report:?}").contains(EDIT_KEY));
        assert_eq!(std::fs::read_to_string(&r.paths.config).unwrap(), before, "untouched");
        assert_eq!(r.old.get(Role::Edit).unwrap().as_deref(), Some(EDIT_KEY), "no key lost");
        assert_eq!(r.keys.get("openrouter").unwrap().as_deref(), Some("sk-someone-elses-key-0000"));
        // And the role still resolves to its own endpoint, with its own key.
        let target = Config::parse(&before).unwrap().context_target(Role::Edit).unwrap();
        assert_eq!(target.key_of, "legacy:edit");
    }

    #[test]
    fn a_key_that_cannot_be_read_leaves_everything_as_it_was() {
        let r = rig("migrate-unreadable", MILESTONE_ONE);
        r.old.set(Role::Edit, EDIT_KEY).unwrap();
        r.store.refuse(Some("the keyring is locked"));
        let before = std::fs::read_to_string(&r.paths.config).unwrap();
        let (report, next) = migrate(&r.paths, &r.old, &r.keys, false).unwrap();
        assert!(next.is_none());
        assert_eq!(report.left.len(), 2);
        assert_eq!(std::fs::read_to_string(&r.paths.config).unwrap(), before);
        r.store.refuse(None);
        assert_eq!(r.old.get(Role::Edit).unwrap().as_deref(), Some(EDIT_KEY));
    }

    #[test]
    fn a_dry_run_writes_nothing_and_a_config_without_the_old_shape_is_left_alone() {
        let r = rig("migrate-dry", MILESTONE_ONE);
        r.old.set(Role::Edit, EDIT_KEY).unwrap();
        let before = std::fs::read_to_string(&r.paths.config).unwrap();
        let (report, next) = migrate(&r.paths, &r.old, &r.keys, true).unwrap();
        assert!(next.is_none() && !report.wrote);
        assert_eq!(report.moves.len(), 2);
        assert_eq!(report.keys_carried, vec![(Role::Edit, "openrouter".to_string())]);
        assert_eq!(std::fs::read_to_string(&r.paths.config).unwrap(), before);
        assert_eq!(r.keys.get("openrouter").unwrap(), None);
        let plain = rig("migrate-none", "[api]\nport = 47201\n");
        let (report, next) = migrate(&plain.paths, &plain.old, &plain.keys, false).unwrap();
        assert!(next.is_none() && report == Report::default());
    }

    #[test]
    fn the_endpoint_is_matched_to_a_kind_only_when_it_plainly_is_one() {
        assert_eq!(base_of(" https://openrouter.ai/api/v1/chat/completions/ "), "https://openrouter.ai/api/v1");
        assert_eq!(kind_of("https://openrouter.ai/api/v1"), Kind::OpenRouter);
        assert_eq!(kind_of("https://api.openai.com/v1"), Kind::OpenAi);
        assert_eq!(kind_of("https://api.anthropic.com/v1"), Kind::Anthropic);
        assert_eq!(kind_of("https://openrouter.ai.example.com/v1"), Kind::Custom);
        assert_eq!(kind_of("http://127.0.0.1:11434/v1"), Kind::Custom);
    }
}
