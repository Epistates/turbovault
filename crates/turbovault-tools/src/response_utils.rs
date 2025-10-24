//! Response utilities for Phase 2 LLMX enhancements
//!
//! Provides:
//! - Rich error responses with recovery guidance
//! - Vault context tracking
//! - Tool chaining suggestions
//! - Batch progress tracking

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Vault context information for responses
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultContext {
    /// Current active vault name
    pub current_vault: String,

    /// Previous vault if switched
    #[serde(skip_serializing_if = "Option::is_none")]
    pub switched_from: Option<String>,

    /// Whether vault was switched back after operation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub switched_back: Option<bool>,
}

/// Batch operation progress tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchProgress {
    /// Unique batch identifier
    pub batch_id: String,

    /// Number of operations completed
    pub completed: u32,

    /// Total number of operations
    pub total: u32,

    /// Progress percentage (0-100)
    pub percentage: u8,

    /// Status of batch operation
    pub status: BatchStatus,

    /// Estimated seconds remaining
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_remaining_seconds: Option<u32>,

    /// Current operation being processed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_operation: Option<String>,
}

/// Batch operation status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum BatchStatus {
    /// Batch is queued, waiting to start
    Queued,
    /// Batch is currently running
    Running,
    /// Batch is paused (can be resumed)
    Paused,
    /// Batch completed successfully
    Completed,
    /// Batch was cancelled by user
    Cancelled,
    /// Batch failed with error
    Failed,
}

impl BatchProgress {
    /// Create new batch progress tracker
    pub fn new(batch_id: String, total: u32) -> Self {
        Self {
            batch_id,
            completed: 0,
            total,
            percentage: 0,
            status: BatchStatus::Queued,
            estimated_remaining_seconds: None,
            current_operation: None,
        }
    }

    /// Update progress
    pub fn update(&mut self, completed: u32, status: BatchStatus) {
        self.completed = completed.min(self.total);
        self.percentage = ((self.completed as f32 / self.total as f32) * 100.0) as u8;
        self.status = status;
    }

    /// Set current operation
    pub fn set_current_operation(&mut self, op: String) {
        self.current_operation = Some(op);
    }

    /// Set estimated remaining time
    pub fn set_estimated_remaining(&mut self, seconds: u32) {
        self.estimated_remaining_seconds = Some(seconds);
    }
}

/// Error response with recovery guidance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// Machine-readable error code
    pub error_code: String,

    /// Root cause explanation
    pub cause: String,

    /// Recovery strategies ranked by success probability
    pub recovery_options: Vec<RecoveryOption>,

    /// Similar errors that might be relevant
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub similar_errors: Vec<String>,

    /// Link to documentation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation_link: Option<String>,

    /// Severity level
    pub severity: ErrorSeverity,
}

/// Suggested recovery action
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryOption {
    /// Recovery strategy description
    pub suggestion: String,

    /// Example of corrected usage
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<String>,

    /// Tool to use for recovery (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,

    /// Estimated success probability (0.0-1.0)
    pub success_probability: f32,
}

/// Error severity level
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ErrorSeverity {
    /// User provided invalid input
    Warning,
    /// Operation failed but vault is consistent
    Error,
    /// Critical issue, vault may be inconsistent
    Critical,
}

/// Suggested next tool to call
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestedTool {
    /// Tool name
    pub tool: String,

    /// Why this tool is suggested
    pub reason: String,

    /// Suggested parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,

    /// Confidence this is the right next step (0.0-1.0)
    pub confidence: f32,
}

/// Response wrapper with Phase 2 enhancements
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhancedResponse {
    /// Core result data
    pub result: Value,

    /// Vault context if vault was involved
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vault_context: Option<VaultContext>,

    /// Error information if operation failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorResponse>,

    /// Batch operation progress if this is a batch operation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_progress: Option<BatchProgress>,

    /// Suggested next tools to call
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub next_suggested_tools: Vec<SuggestedTool>,

    /// Explanation of the tool chain
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_explanation: Option<String>,

    /// How long the operation took
    pub execution_time_ms: u64,

    /// When the operation completed
    pub timestamp: String,
}

impl EnhancedResponse {
    /// Create successful response with result
    pub fn success(result: Value) -> Self {
        Self {
            result,
            vault_context: None,
            error: None,
            batch_progress: None,
            next_suggested_tools: Vec::new(),
            chain_explanation: None,
            execution_time_ms: 0,
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Add error information to response
    pub fn with_error(mut self, error: ErrorResponse) -> Self {
        self.error = Some(error);
        self
    }

    /// Add vault context to response
    pub fn with_vault_context(mut self, context: VaultContext) -> Self {
        self.vault_context = Some(context);
        self
    }

    /// Add batch progress tracking
    pub fn with_batch_progress(mut self, progress: BatchProgress) -> Self {
        self.batch_progress = Some(progress);
        self
    }

    /// Add suggested next tools
    pub fn with_suggestions(mut self, suggestions: Vec<SuggestedTool>) -> Self {
        self.next_suggested_tools = suggestions;
        self
    }

    /// Add chain explanation
    pub fn with_chain_explanation(mut self, explanation: String) -> Self {
        self.chain_explanation = Some(explanation);
        self
    }

    /// Set execution time
    pub fn with_execution_time(mut self, ms: u64) -> Self {
        self.execution_time_ms = ms;
        self
    }

    /// Convert to JSON string for MCP response
    pub fn to_json_string(&self) -> String {
        serde_json::to_string(self)
            .unwrap_or_else(|_| json!({ "error": "Failed to serialize response" }).to_string())
    }
}

/// Error builder for Phase 2 error responses
pub struct ErrorBuilder {
    error_code: String,
    cause: String,
    severity: ErrorSeverity,
    recovery_options: Vec<RecoveryOption>,
    similar_errors: Vec<String>,
    documentation_link: Option<String>,
}

impl ErrorBuilder {
    /// Create new error builder
    pub fn new(error_code: &str, cause: &str) -> Self {
        Self {
            error_code: error_code.to_string(),
            cause: cause.to_string(),
            severity: ErrorSeverity::Error,
            recovery_options: Vec::new(),
            similar_errors: Vec::new(),
            documentation_link: None,
        }
    }

    /// Set severity level
    pub fn severity(mut self, severity: ErrorSeverity) -> Self {
        self.severity = severity;
        self
    }

    /// Add recovery option
    pub fn add_recovery(mut self, suggestion: &str, success_probability: f32) -> Self {
        self.recovery_options.push(RecoveryOption {
            suggestion: suggestion.to_string(),
            example: None,
            tool: None,
            success_probability,
        });
        self
    }

    /// Add recovery with example
    pub fn add_recovery_with_example(
        mut self,
        suggestion: &str,
        example: &str,
        success_probability: f32,
    ) -> Self {
        self.recovery_options.push(RecoveryOption {
            suggestion: suggestion.to_string(),
            example: Some(example.to_string()),
            tool: None,
            success_probability,
        });
        self
    }

    /// Add recovery with tool suggestion
    pub fn add_recovery_with_tool(
        mut self,
        suggestion: &str,
        tool: &str,
        success_probability: f32,
    ) -> Self {
        self.recovery_options.push(RecoveryOption {
            suggestion: suggestion.to_string(),
            example: None,
            tool: Some(tool.to_string()),
            success_probability,
        });
        self
    }

    /// Add similar error code
    pub fn add_similar_error(mut self, error_code: &str) -> Self {
        self.similar_errors.push(error_code.to_string());
        self
    }

    /// Set documentation link
    pub fn with_documentation(mut self, link: &str) -> Self {
        self.documentation_link = Some(link.to_string());
        self
    }

    /// Build the error response
    pub fn build(self) -> ErrorResponse {
        ErrorResponse {
            error_code: self.error_code,
            cause: self.cause,
            recovery_options: self.recovery_options,
            similar_errors: self.similar_errors,
            documentation_link: self.documentation_link,
            severity: self.severity,
        }
    }
}

/// Common error responses
pub mod errors {
    use super::*;

    /// Path traversal attempt
    pub fn path_traversal(requested: &str, _vault_root: &str) -> ErrorResponse {
        ErrorBuilder::new(
            "PATH_TRAVERSAL",
            &format!("Requested path '{}' escapes vault boundary", requested),
        )
        .severity(ErrorSeverity::Error)
        .add_recovery_with_example(
            "Use relative path within vault",
            "read_note(path='folder/note')",
            0.95,
        )
        .add_recovery_with_tool(
            "List available files to find correct path",
            "list_files",
            0.8,
        )
        .add_recovery_with_tool(
            "Check vault configuration and root",
            "get_vault_context",
            0.7,
        )
        .add_similar_error("INVALID_PATH")
        .add_similar_error("VAULT_NOT_INITIALIZED")
        .with_documentation("Security#Path-Traversal-Protection")
        .build()
    }

    /// Missing required parameter
    pub fn missing_parameter(param_name: &str, operation: &str) -> ErrorResponse {
        ErrorBuilder::new(
            "MISSING_REQUIRED_PARAMETER",
            &format!(
                "Required parameter '{}' missing for {}",
                param_name, operation
            ),
        )
        .severity(ErrorSeverity::Warning)
        .add_recovery_with_example(
            "Provide the missing parameter",
            &format!("{}({}=value)", operation, param_name),
            0.99,
        )
        .add_recovery_with_tool(
            "Get guidance on how to use this operation",
            "get_tool_guidance",
            0.7,
        )
        .with_documentation("Tools-Reference#parameters")
        .build()
    }

    /// File not found
    pub fn file_not_found(path: &str) -> ErrorResponse {
        ErrorBuilder::new("FILE_NOT_FOUND", &format!("Note '{}' does not exist", path))
            .severity(ErrorSeverity::Warning)
            .add_recovery_with_tool("Search for similar notes", "search", 0.8)
            .add_recovery_with_tool("List all files in vault", "list_files", 0.9)
            .add_recovery_with_example(
                "Create the note if intended",
                &format!("write_note(path='{}', content='...')", path),
                0.7,
            )
            .add_similar_error("INVALID_PATH")
            .build()
    }

    /// Vault not found
    pub fn vault_not_found(vault_name: &str) -> ErrorResponse {
        ErrorBuilder::new(
            "VAULT_NOT_FOUND",
            &format!("Vault '{}' is not registered", vault_name),
        )
        .severity(ErrorSeverity::Error)
        .add_recovery_with_tool("List all registered vaults", "list_vaults", 0.95)
        .add_recovery_with_tool("Register the vault", "add_vault", 0.9)
        .add_similar_error("VAULT_NOT_INITIALIZED")
        .with_documentation("Multi-Vault#vault-management")
        .build()
    }

    /// Operation timeout
    pub fn operation_timeout(operation: &str, timeout_ms: u64) -> ErrorResponse {
        ErrorBuilder::new(
            "OPERATION_TIMEOUT",
            &format!("{} exceeded timeout of {}ms", operation, timeout_ms),
        )
        .severity(ErrorSeverity::Error)
        .add_recovery_with_tool(
            "Use batch progress monitoring for long operations",
            "batch_execute",
            0.7,
        )
        .add_recovery_with_tool("Try a simpler query with fewer results", "search", 0.6)
        .add_recovery("Increase timeout if available", 0.5)
        .with_documentation("Performance#Tuning")
        .build()
    }
}

/// Suggestion builders for tool chaining
pub mod suggestions {
    use super::*;

    /// After querying metadata, suggest organization or batch operations
    pub fn after_query_metadata(match_count: usize) -> Vec<SuggestedTool> {
        vec![
            SuggestedTool {
                tool: "organize_by_metadata".to_string(),
                reason: format!("Move these {} matched notes to folder", match_count),
                parameters: Some(json!({
                    "pattern": "/* same pattern as query */",
                    "destination": "/* target folder */"
                })),
                confidence: 0.95,
            },
            SuggestedTool {
                tool: "batch_execute".to_string(),
                reason: "Perform custom operations on matched notes".to_string(),
                parameters: None,
                confidence: 0.7,
            },
            SuggestedTool {
                tool: "export_vault_stats".to_string(),
                reason: "Generate report with current statistics".to_string(),
                parameters: Some(json!({ "format": "json" })),
                confidence: 0.5,
            },
        ]
    }

    /// After reading note, suggest related operations
    pub fn after_read_note() -> Vec<SuggestedTool> {
        vec![
            SuggestedTool {
                tool: "write_note".to_string(),
                reason: "Modify and save changes to this note".to_string(),
                parameters: None,
                confidence: 0.8,
            },
            SuggestedTool {
                tool: "get_backlinks".to_string(),
                reason: "See which notes reference this one".to_string(),
                parameters: None,
                confidence: 0.7,
            },
            SuggestedTool {
                tool: "get_related_notes".to_string(),
                reason: "Explore conceptually related notes".to_string(),
                parameters: None,
                confidence: 0.6,
            },
        ]
    }

    /// After audit, suggest fixes
    pub fn after_audit() -> Vec<SuggestedTool> {
        vec![
            SuggestedTool {
                tool: "get_broken_links".to_string(),
                reason: "Get list of broken links to fix".to_string(),
                parameters: None,
                confidence: 0.95,
            },
            SuggestedTool {
                tool: "organize_by_metadata".to_string(),
                reason: "Reorganize vault structure based on issues found".to_string(),
                parameters: None,
                confidence: 0.7,
            },
            SuggestedTool {
                tool: "export_health_report".to_string(),
                reason: "Export audit results for documentation".to_string(),
                parameters: Some(json!({ "format": "json" })),
                confidence: 0.6,
            },
        ]
    }

    /// After search, suggest inspection or organization
    pub fn after_search(result_count: usize) -> Vec<SuggestedTool> {
        vec![
            SuggestedTool {
                tool: "read_note".to_string(),
                reason: "Inspect top search result for details".to_string(),
                parameters: None,
                confidence: 0.9,
            },
            SuggestedTool {
                tool: "get_related_notes".to_string(),
                reason: "Explore conceptually related notes".to_string(),
                parameters: None,
                confidence: 0.7,
            },
            if result_count > 1 {
                SuggestedTool {
                    tool: "batch_execute".to_string(),
                    reason: format!("Perform bulk operations on {} results", result_count),
                    parameters: None,
                    confidence: 0.6,
                }
            } else {
                SuggestedTool {
                    tool: "suggest_links".to_string(),
                    reason: "Find other notes to link with".to_string(),
                    parameters: None,
                    confidence: 0.6,
                }
            },
        ]
    }

    /// After writing/creating note, suggest linking
    pub fn after_write_note() -> Vec<SuggestedTool> {
        vec![
            SuggestedTool {
                tool: "suggest_links".to_string(),
                reason: "Find other notes to link with this one".to_string(),
                parameters: None,
                confidence: 0.85,
            },
            SuggestedTool {
                tool: "get_related_notes".to_string(),
                reason: "Discover conceptually related notes".to_string(),
                parameters: None,
                confidence: 0.7,
            },
            SuggestedTool {
                tool: "read_note".to_string(),
                reason: "Verify the note was created correctly".to_string(),
                parameters: None,
                confidence: 0.6,
            },
        ]
    }

    /// After organizing notes, suggest verification
    pub fn after_organize() -> Vec<SuggestedTool> {
        vec![
            SuggestedTool {
                tool: "quick_health_check".to_string(),
                reason: "Verify vault health after reorganization".to_string(),
                parameters: None,
                confidence: 0.9,
            },
            SuggestedTool {
                tool: "get_broken_links".to_string(),
                reason: "Check for broken links from moved notes".to_string(),
                parameters: None,
                confidence: 0.7,
            },
            SuggestedTool {
                tool: "list_files".to_string(),
                reason: "Verify new folder structure".to_string(),
                parameters: None,
                confidence: 0.6,
            },
        ]
    }

    /// After health check, suggest deep analysis
    pub fn after_health_check(is_healthy: bool) -> Vec<SuggestedTool> {
        if is_healthy {
            vec![
                SuggestedTool {
                    tool: "export_vault_stats".to_string(),
                    reason: "Export statistics to document vault health".to_string(),
                    parameters: Some(json!({ "format": "json" })),
                    confidence: 0.7,
                },
                SuggestedTool {
                    tool: "get_centrality_ranking".to_string(),
                    reason: "Find the most important/central notes".to_string(),
                    parameters: None,
                    confidence: 0.6,
                },
            ]
        } else {
            vec![
                SuggestedTool {
                    tool: "full_health_analysis".to_string(),
                    reason: "Get detailed breakdown of health issues".to_string(),
                    parameters: None,
                    confidence: 0.95,
                },
                SuggestedTool {
                    tool: "get_broken_links".to_string(),
                    reason: "Identify broken links to fix".to_string(),
                    parameters: None,
                    confidence: 0.85,
                },
                SuggestedTool {
                    tool: "audit_vault".to_string(),
                    reason: "Comprehensive audit with recommendations".to_string(),
                    parameters: None,
                    confidence: 0.8,
                },
            ]
        }
    }

    /// After moving/organizing, suggest verification
    pub fn after_move_note() -> Vec<SuggestedTool> {
        vec![
            SuggestedTool {
                tool: "read_note".to_string(),
                reason: "Verify note is in correct location".to_string(),
                parameters: None,
                confidence: 0.8,
            },
            SuggestedTool {
                tool: "get_backlinks".to_string(),
                reason: "Check that backlinks still work after move".to_string(),
                parameters: None,
                confidence: 0.75,
            },
            SuggestedTool {
                tool: "quick_health_check".to_string(),
                reason: "Verify vault integrity after move".to_string(),
                parameters: None,
                confidence: 0.6,
            },
        ]
    }

    /// After batch_execute starts, suggest monitoring tools
    pub fn after_batch_start() -> Vec<SuggestedTool> {
        vec![
            SuggestedTool {
                tool: "get_batch_status".to_string(),
                reason: "Monitor batch progress and completion status".to_string(),
                parameters: None,
                confidence: 0.95,
            },
            SuggestedTool {
                tool: "cancel_batch".to_string(),
                reason: "Cancel the batch if needed".to_string(),
                parameters: None,
                confidence: 0.6,
            },
        ]
    }

    /// Generic suggestion for any tool
    pub fn generic() -> Vec<SuggestedTool> {
        vec![SuggestedTool {
            tool: "get_vault_schema".to_string(),
            reason: "Understand vault structure and conventions".to_string(),
            parameters: None,
            confidence: 0.5,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_builder() {
        let error = ErrorBuilder::new("TEST_ERROR", "Test cause")
            .severity(ErrorSeverity::Error)
            .add_recovery("Try this", 0.9)
            .add_similar_error("SIMILAR_ERROR")
            .build();

        assert_eq!(error.error_code, "TEST_ERROR");
        assert_eq!(error.cause, "Test cause");
        assert_eq!(error.recovery_options.len(), 1);
        assert_eq!(error.similar_errors.len(), 1);
    }

    #[test]
    fn test_enhanced_response() {
        let response =
            EnhancedResponse::success(json!({"data": "test"})).with_vault_context(VaultContext {
                current_vault: "default".to_string(),
                switched_from: None,
                switched_back: None,
            });

        assert_eq!(
            response.vault_context.as_ref().unwrap().current_vault,
            "default"
        );
        assert!(response.next_suggested_tools.is_empty());
    }
}
