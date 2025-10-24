//! Shared utilities for operations across turbovault crates.
//!
//! Provides DRY helpers for:
//! - Serialization with consistent error handling
//! - Result/report builders
//! - Path validation
//! - Transaction tracking

use crate::{Error, Result};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Generic JSON serialization with consistent error handling
/// Works with any type that implements Serialize (including slices)
pub fn to_json_string<T: serde::Serialize + ?Sized>(data: &T, context: &str) -> Result<String> {
    serde_json::to_string_pretty(data).map_err(|e| {
        Error::config_error(format!("Failed to serialize {} as JSON: {}", context, e))
    })
}

/// Generic CSV serialization builder
/// Use the CSVBuilder fluent API to construct and export CSV data
pub struct CSVBuilder {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl CSVBuilder {
    /// Create a new CSV with headers
    pub fn new(headers: Vec<&str>) -> Self {
        Self {
            headers: headers.iter().map(|s| s.to_string()).collect(),
            rows: Vec::new(),
        }
    }

    /// Add a row of data
    pub fn add_row(mut self, values: Vec<&str>) -> Self {
        self.rows.push(values.iter().map(|s| s.to_string()).collect());
        self
    }

    /// Add a row of data from owned strings
    pub fn add_row_owned(mut self, values: Vec<String>) -> Self {
        self.rows.push(values);
        self
    }

    /// Build the CSV string
    pub fn build(self) -> String {
        let mut csv = self.headers.join(",") + "\n";
        for row in self.rows {
            csv.push_str(&row.join(","));
            csv.push('\n');
        }
        csv
    }
}

/// Path validation helpers
pub struct PathValidator;

impl PathValidator {
    /// Ensure a path is within a vault root (prevents directory traversal)
    pub fn validate_path_in_vault(vault_root: &Path, path: &Path) -> Result<PathBuf> {
        let full_path = vault_root.join(path);

        // Canonicalize would require the path to exist. Instead, we check if
        // the normalized path is still within vault_root by comparing components.
        let canonical_vault = vault_root.canonicalize()
            .unwrap_or_else(|_| vault_root.to_path_buf());

        // For non-existent paths, at least check that it doesn't escape via ..
        // by ensuring normalized form would still be under vault
        if let Ok(canonical_full) = full_path.canonicalize() {
            if !canonical_full.starts_with(&canonical_vault) {
                return Err(Error::path_traversal(full_path));
            }
        } else {
            // Path doesn't exist, check statically using path normalization
            use std::path::Component;
            let mut normalized = PathBuf::new();
            for component in full_path.components() {
                match component {
                    Component::ParentDir => {
                        normalized.pop();
                    }
                    Component::Normal(name) => {
                        normalized.push(name);
                    }
                    Component::RootDir => {
                        normalized.push(component);
                    }
                    Component::CurDir => {
                        // Skip .
                    }
                    Component::Prefix(p) => {
                        normalized.push(p.as_os_str());
                    }
                }
            }

            if !normalized.starts_with(vault_root) {
                return Err(Error::path_traversal(full_path));
            }
        }

        Ok(full_path)
    }

    /// Ensure a path exists in the vault
    pub fn validate_path_exists(vault_root: &Path, path: &Path) -> Result<PathBuf> {
        let full_path = Self::validate_path_in_vault(vault_root, path)?;
        if !full_path.exists() {
            return Err(Error::file_not_found(&full_path));
        }
        Ok(full_path)
    }

    /// Get multiple paths and validate them all
    pub fn validate_multiple(vault_root: &Path, paths: &[&str]) -> Result<Vec<PathBuf>> {
        paths
            .iter()
            .map(|p| Self::validate_path_in_vault(vault_root, Path::new(p)))
            .collect()
    }
}

/// Transaction tracking utilities
pub struct TransactionBuilder {
    transaction_id: String,
    start_time: Instant,
}

impl TransactionBuilder {
    /// Create a new transaction tracker
    pub fn new() -> Self {
        Self {
            transaction_id: uuid::Uuid::new_v4().to_string(),
            start_time: Instant::now(),
        }
    }

    /// Get the transaction ID
    pub fn transaction_id(&self) -> &str {
        &self.transaction_id
    }

    /// Get elapsed time in milliseconds
    pub fn elapsed_ms(&self) -> u64 {
        self.start_time.elapsed().as_millis() as u64
    }
}

impl Default for TransactionBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    struct TestData {
        name: String,
        value: i32,
    }

    #[test]
    fn test_to_json_string() {
        let data = TestData {
            name: "test".to_string(),
            value: 42,
        };
        let json = to_json_string(&data, "test_data").unwrap();
        assert!(json.contains("test"));
        assert!(json.contains("42"));
    }

    #[test]
    fn test_csv_builder() {
        let csv = CSVBuilder::new(vec!["name", "age"])
            .add_row(vec!["Alice", "30"])
            .add_row(vec!["Bob", "25"])
            .build();

        assert!(csv.contains("name,age"));
        assert!(csv.contains("Alice,30"));
        assert!(csv.contains("Bob,25"));
    }

    #[test]
    fn test_path_validator_valid() {
        let vault_root = PathBuf::from("/vault");
        let path = Path::new("notes/file.md");
        let result = PathValidator::validate_path_in_vault(&vault_root, path);
        assert!(result.is_ok());
    }

    #[test]
    fn test_path_validator_traversal() {
        let vault_root = PathBuf::from("/vault");
        let path = Path::new("../../../etc/passwd");
        let result = PathValidator::validate_path_in_vault(&vault_root, path);
        assert!(result.is_err());
    }

    #[test]
    fn test_transaction_builder() {
        let builder = TransactionBuilder::new();
        assert!(!builder.transaction_id().is_empty());
        let elapsed = builder.elapsed_ms();
        assert!(elapsed < 1000); // Should complete in less than 1 second
    }
}
