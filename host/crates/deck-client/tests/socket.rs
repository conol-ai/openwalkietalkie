//! `default_socket()` honours `DECKD_DATA_DIR` (contract §3.5), the same
//! rule as `deckd`. Its own process: the environment is process-global.
use deck_client::default_socket;

#[test]
fn default_socket_honours_deckd_data_dir() {
    std::env::set_var("DECKD_DATA_DIR", "/tmp/dc-x");
    assert_eq!(
        default_socket(),
        std::path::PathBuf::from("/tmp/dc-x/deckd.sock")
    );
    std::env::remove_var("DECKD_DATA_DIR");
    let path = default_socket();
    assert!(path.ends_with("deckd.sock"));
    assert!(!path.starts_with("/tmp/dc-x"));
    if cfg!(target_os = "macos") {
        assert!(path
            .to_string_lossy()
            .ends_with("Library/Application Support/OpenWalkieTalkie/deckd.sock"));
    } else {
        // The daemon's own rule elsewhere, read from this process's
        // environment: the runtime dir, else the cache dir.
        std::env::set_var("XDG_RUNTIME_DIR", "/run/user/4242");
        assert_eq!(
            default_socket(),
            std::path::PathBuf::from("/run/user/4242/openwalkietalkie/deckd.sock")
        );
        std::env::remove_var("XDG_RUNTIME_DIR");
        std::env::set_var("XDG_CACHE_HOME", "/tmp/dc-cache");
        assert_eq!(
            default_socket(),
            std::path::PathBuf::from("/tmp/dc-cache/openwalkietalkie/deckd.sock")
        );
        std::env::remove_var("XDG_CACHE_HOME");
    }
}
