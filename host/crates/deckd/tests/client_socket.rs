//! The socket a client looks for is the one the daemon listens on.
//! `deck_client::default_socket` writes the daemon's rule out again (it
//! cannot depend on `deckd`), so this row holds the two against each other:
//! `deckd::config::Paths::detect` against the client's answer, under the
//! data-dir override, a runtime dir, a cache dir, a relative XDG variable
//! (which neither counts) and none at all. On macOS every case but the
//! first is the Library folder; on Linux each is a different branch.
//!
//! A process of its own, because the environment is process-global. It only
//! resolves paths: nothing is created, read or bound, and `HOME` names a
//! directory that need not exist.

/// The variables one case sets (`Some`) or takes away (`None`), in order.
type Vars = &'static [(&'static str, Option<&'static str>)];

#[test]
fn the_client_looks_where_the_daemon_listens() {
    let home = std::env::temp_dir().join(format!("dkcs{}-home", std::process::id()));
    std::env::set_var("HOME", &home);
    let cases: [(&str, Vars); 5] = [
        (
            "the data dir",
            &[
                ("DECKD_DATA_DIR", Some("/tmp/dkcs-data")),
                ("XDG_RUNTIME_DIR", Some("/tmp/dkcs-run")),
            ],
        ),
        (
            "a runtime dir",
            &[
                ("DECKD_DATA_DIR", None),
                ("XDG_RUNTIME_DIR", Some("/tmp/dkcs-run")),
                ("XDG_CACHE_HOME", Some("/tmp/dkcs-cache")),
            ],
        ),
        (
            "a cache dir and no runtime dir",
            &[
                ("XDG_RUNTIME_DIR", None),
                ("XDG_CACHE_HOME", Some("/tmp/dkcs-cache")),
            ],
        ),
        (
            "relative XDG variables",
            &[
                ("XDG_RUNTIME_DIR", Some("run")),
                ("XDG_CACHE_HOME", Some("cache")),
            ],
        ),
        (
            "no XDG variable",
            &[("XDG_RUNTIME_DIR", None), ("XDG_CACHE_HOME", None)],
        ),
    ];
    for (case, env) in cases {
        for (name, value) in env {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        let daemon = deckd::config::Paths::detect()
            .unwrap_or_else(|e| panic!("{case}: the daemon resolves its paths: {e}"))
            .socket;
        assert_eq!(deck_client::default_socket(), daemon, "{case}");
    }
    // The first case is the override on every platform; the last is where
    // a plain Linux `deckd` listens, and on macOS still the Library folder.
    let last = deck_client::default_socket();
    if cfg!(target_os = "macos") {
        assert_eq!(
            last,
            home.join("Library/Application Support/OpenWalkieTalkie/deckd.sock")
        );
    } else {
        assert_eq!(last, home.join(".cache/openwalkietalkie/deckd.sock"));
    }
}
