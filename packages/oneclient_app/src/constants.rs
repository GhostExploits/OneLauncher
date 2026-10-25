pub const WINDOW_APP_ID: &str = "org.spiderclient.SpiderClient";
pub const WINDOW_TITLE: &str = "SpiderClient";

/// The public key is shared with the upstream release workflow. SpiderClient
/// releases must be signed with the matching private key before this endpoint
/// can serve an automatic update. Pointing the desktop build at the upstream
/// OneClient manifest would install the wrong product over this fork.
pub const UPDATER_PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDFGODk3MkMyMjg0MjFDMDUKUldRRkhFSW93bktKSHpkWjNEMXNzaDVINVpCTU8xSnhuK2RnV0dTZ2FkcFJWbG1zUkhGYTNjaUkK";
pub const UPDATER_ENDPOINT: &str =
    "https://github.com/GhostExploits/OneLauncher/releases/latest/download/latest.json";
pub const RELEASES_URL: &str = "https://github.com/GhostExploits/OneLauncher/releases/latest";
