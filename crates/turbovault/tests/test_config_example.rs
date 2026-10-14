//! `config.example.yaml` is what users copy, so it must load the way a real
//! config does: every vault, the tool visibility section and the security
//! section, with no key the loaders refuse.

use turbovault::security::SecuritySettings;
use turbovault::tool_visibility::TurboVaultConfigFile;

const EXAMPLE: &str = include_str!("../../../config.example.yaml");

#[test]
fn the_example_config_loads() {
    let config = TurboVaultConfigFile::from_yaml_str(EXAMPLE).expect("vaults and tool_visibility");
    assert_eq!(config.vaults.len(), 1);
    let vault = &config.vaults[0];
    assert_eq!(vault.name, "personal");
    vault
        .validate_operations()
        .expect("allowed_operations names real operations");

    let security = SecuritySettings::from_yaml_str(EXAMPLE).expect("security section");
    assert_eq!(security.allowed_vault_roots.len(), 1);
}
