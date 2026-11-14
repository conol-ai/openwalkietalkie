//! Principals and scope resolution (design §4.2, contract §3.4): what a
//! peer asked for against what it may have (the UDS maximum expanded from
//! `[api] uds_max_scope`, the ancestry cap, a token's scopes), explicit
//! refusal instead of a silent intersection, and the bearer/token lookup.
pub use crate::core::handle::ClientInfo;
use crate::tokens::Tokens;
use deck_model::{ApiError, Principal, Scope, Ts};

/// `admin` expands to every scope but `observe:raw`; any other name is
/// itself; `observe:raw` only when listed explicitly.
pub fn expand_scope(name: &str) -> Result<Vec<Scope>, ApiError> {
    let scope: Scope = serde_json::from_value(serde_json::Value::String(name.to_string()))
        .map_err(|_| ApiError::invalid("scopes", &format!("unknown scope {name:?}")))?;
    Ok(match scope {
        Scope::Admin => Scope::ALL
            .iter()
            .copied()
            .filter(|s| *s != Scope::ObserveRaw)
            .collect(),
        other => vec![other],
    })
}

/// A comma-separated or listed set of scope names, expanded and deduplicated.
pub fn expand_scopes(names: &[String]) -> Result<Vec<Scope>, ApiError> {
    let mut out: Vec<Scope> = Vec::new();
    for name in names {
        for scope in expand_scope(name.trim())? {
            if !out.contains(&scope) {
                out.push(scope);
            }
        }
    }
    Ok(out)
}

/// The scopes a hello asks for, one per name, deduplicated, never expanded
/// (`admin` stays `admin`: what was asked is what is refused or granted).
pub fn parse_scopes(names: &[String]) -> Result<Vec<Scope>, ApiError> {
    let mut out: Vec<Scope> = Vec::new();
    for name in names {
        let name = name.trim();
        let scope: Scope = serde_json::from_value(serde_json::Value::String(name.to_string()))
            .map_err(|_| ApiError::invalid("scopes", &format!("unknown scope {name:?}")))?;
        if !out.contains(&scope) {
            out.push(scope);
        }
    }
    Ok(out)
}

/// `requested ∩ maximum`, refused explicitly (`-32002 forbidden{need}`)
/// when `requested` holds a scope outside `maximum`; when `capped`, the
/// maximum is first cut to `Scope::AGENT_DESCENDED_CAP` and a scope the
/// cap removed is refused with `reason:"agent_ancestry"`.
pub fn resolve(
    requested: &[Scope],
    maximum: &[Scope],
    capped: bool,
) -> Result<Vec<Scope>, ApiError> {
    let mut granted = Vec::with_capacity(requested.len());
    for scope in requested {
        let name = scope_name(*scope);
        if !maximum.contains(scope) {
            return Err(ApiError::forbidden(&name));
        }
        if capped && !Scope::AGENT_DESCENDED_CAP.contains(scope) {
            return Err(ApiError::forbidden_ancestry(&name));
        }
        if !granted.contains(scope) {
            granted.push(*scope);
        }
    }
    Ok(granted)
}

/// The wire name of a scope (`"observe:content"`).
pub fn scope_name(scope: Scope) -> String {
    serde_json::to_value(scope)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

/// Whether `have` satisfies `need`: itself, or `admin` for anything but
/// `observe:raw`, or a content scope for `observe`.
pub fn has_scope(have: &[Scope], need: Scope) -> bool {
    if have.contains(&need) {
        return true;
    }
    if need != Scope::ObserveRaw && have.contains(&Scope::Admin) {
        return true;
    }
    need == Scope::Observe && Scope::grants_content(have)
}

/// The daemon's uid: the only uid a UDS peer may have.
#[cfg(unix)]
pub fn own_uid() -> u32 {
    // SAFETY: getuid has no preconditions.
    unsafe { libc::getuid() }
}

/// The daemon's "uid" on Windows: its user SID's relative id (1001 for the
/// first local account), which is what `uid:<n>` prints. The pipe door
/// compares whole SIDs; this number only names the principal.
#[cfg(windows)]
pub fn own_uid() -> u32 {
    deck_client::windows::current_user()
        .map(|user| user.rid())
        .unwrap_or(0)
}

/// The principal of a UDS peer.
pub fn uds_principal(uid: u32) -> Principal {
    Principal::Uid(uid)
}

/// `Authorization: Bearer dk_…` → the plaintext.
pub fn parse_bearer(header: &str) -> Option<&str> {
    let (scheme, rest) = header.trim().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = rest.trim();
    (!token.is_empty()).then_some(token)
}

/// A presented token → its principal and scopes, when it verifies.
pub fn token_principal(
    tokens: &Tokens,
    plaintext: &str,
    now: Ts,
) -> Option<(Principal, Vec<Scope>)> {
    let token = tokens.verify(plaintext, now)?;
    Some((Principal::Token(token.name.clone()), token.scopes.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_expands_to_everything_but_raw_and_names_expand_to_themselves() {
        let admin = expand_scope("admin").unwrap();
        assert_eq!(admin.len(), Scope::ALL.len() - 1);
        assert!(!admin.contains(&Scope::ObserveRaw));
        assert!(admin.contains(&Scope::Admin) && admin.contains(&Scope::Operate));
        assert_eq!(
            expand_scope("observe:raw").unwrap(),
            vec![Scope::ObserveRaw]
        );
        assert_eq!(expand_scope("observe").unwrap(), vec![Scope::Observe]);
        let err = expand_scope("root").unwrap_err();
        assert_eq!(err.code, ApiError::INVALID);
        let both = expand_scopes(&["observe".into(), " agent".into(), "observe".into()]).unwrap();
        assert_eq!(both, vec![Scope::Observe, Scope::Agent]);
        assert_eq!(
            parse_scopes(&["admin".into(), "admin".into(), " voice ".into()]).unwrap(),
            vec![Scope::Admin, Scope::Voice],
            "a hello's scopes are taken literally"
        );
        assert!(parse_scopes(&["root".into()]).is_err());
        assert_eq!(scope_name(Scope::ObserveContent), "observe:content");
        assert_eq!(scope_name(Scope::HooksIngest), "hooks:ingest");
    }

    #[test]
    fn resolution_refuses_explicitly_and_the_ancestry_cap_names_its_reason() {
        let max = expand_scope("admin").unwrap();
        assert_eq!(
            resolve(&[Scope::Observe, Scope::Agent, Scope::Agent], &max, false).unwrap(),
            vec![Scope::Observe, Scope::Agent]
        );
        let err = resolve(&[Scope::Observe, Scope::ObserveRaw], &max, false).unwrap_err();
        assert_eq!(err.code, ApiError::FORBIDDEN);
        assert_eq!(err.data, serde_json::json!({"need": "observe:raw"}));
        let err = resolve(&[Scope::Operate], &max, true).unwrap_err();
        assert_eq!(err.data["reason"], "agent_ancestry");
        assert_eq!(err.data["need"], "operate");
        assert_eq!(
            resolve(
                &[Scope::Agent, Scope::Voice, Scope::HooksIngest],
                &max,
                true
            )
            .unwrap(),
            vec![Scope::Agent, Scope::Voice, Scope::HooksIngest]
        );
        // A token's maximum is its own list; the cap never applies over HTTP.
        let token_max = vec![Scope::Observe, Scope::Voice];
        assert!(resolve(&[Scope::Agent], &token_max, false).is_err());
        assert_eq!(
            resolve(&[], &token_max, false).unwrap(),
            Vec::<Scope>::new()
        );
        // A scope outside the maximum is `forbidden`, not `agent_ancestry`, even when capped.
        let err = resolve(&[Scope::ObserveRaw], &max, true).unwrap_err();
        assert!(err.data.get("reason").is_none());
    }

    #[test]
    fn scope_checks_imply_observe_from_content_and_everything_from_admin() {
        assert!(has_scope(&[Scope::Observe], Scope::Observe));
        assert!(!has_scope(&[Scope::Observe], Scope::Operate));
        assert!(has_scope(&[Scope::Operate], Scope::Observe));
        assert!(has_scope(&[Scope::ObserveContent], Scope::Observe));
        assert!(!has_scope(&[Scope::Agent], Scope::Observe));
        assert!(has_scope(&[Scope::Admin], Scope::Device));
        assert!(has_scope(&[Scope::Admin], Scope::Admin));
        assert!(!has_scope(&[Scope::Admin], Scope::ObserveRaw));
        assert!(has_scope(&[Scope::ObserveRaw], Scope::ObserveRaw));
        assert!(!has_scope(&[], Scope::Observe));
    }

    #[test]
    fn bearers_parse_and_tokens_resolve_to_their_principal() {
        assert_eq!(parse_bearer("Bearer dk_abc"), Some("dk_abc"));
        assert_eq!(parse_bearer("bearer  dk_abc "), Some("dk_abc"));
        assert_eq!(parse_bearer("Basic dk_abc"), None);
        assert_eq!(parse_bearer("Bearer"), None);
        assert_eq!(parse_bearer("Bearer "), None);
        let now = Ts(1_789_760_523_000);
        let mut tokens = Tokens::default();
        let plain = tokens
            .create("gate", vec![Scope::Observe, Scope::Voice], None, None, now)
            .unwrap();
        let (principal, scopes) = token_principal(&tokens, &plain, now).unwrap();
        assert_eq!(principal, Principal::Token("gate".into()));
        assert_eq!(scopes, vec![Scope::Observe, Scope::Voice]);
        assert!(token_principal(&tokens, "dk_nope", now).is_none());
        assert_eq!(uds_principal(own_uid()), Principal::Uid(own_uid()));
    }
}
