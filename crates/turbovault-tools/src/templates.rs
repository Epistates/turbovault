//! Template system for LLM-managed vault notes
//!
//! Enables LLMs to:
//! - Discover available templates
//! - Understand expected structure
//! - Create consistently-formatted notes
//! - Validate notes against templates

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use turbovault_vault::VaultManager;

/// Field types for template parameters
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TemplateFieldType {
    Text,
    LongText,
    Date,
    Select(Vec<String>), // options
    MultiSelect(Vec<String>),
    Number,
    Boolean,
}

/// Template field definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateField {
    pub name: String,
    pub description: String,
    pub field_type: TemplateFieldType,
    pub required: bool,
    pub default_value: Option<String>,
    pub example: Option<String>,
}

/// Complete template definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateDefinition {
    /// Template identifier (e.g., "feature-doc", "bug-report")
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Description of when/how to use
    pub description: String,
    /// Category (e.g., "documentation", "tasks", "research")
    pub category: String,
    /// Front matter template (YAML)
    pub frontmatter_template: HashMap<String, String>,
    /// Fields LLM must fill in
    pub fields: Vec<TemplateField>,
    /// Content template with placeholders like {field_name}
    pub content_template: String,
    /// Example note created from this template
    pub example_output: String,
}

impl TemplateDefinition {
    /// Create builder for templates
    pub fn builder(id: impl Into<String>, name: impl Into<String>) -> TemplateBuilder {
        TemplateBuilder {
            id: id.into(),
            name: name.into(),
            description: String::new(),
            category: "uncategorized".to_string(),
            frontmatter_template: HashMap::new(),
            fields: Vec::new(),
            content_template: String::new(),
            example_output: String::new(),
        }
    }

    /// Get all required fields
    pub fn required_fields(&self) -> Vec<&TemplateField> {
        self.fields.iter().filter(|f| f.required).collect()
    }

    /// Validate field value against type
    pub fn validate_field(&self, field_name: &str, value: &str) -> Result<(), String> {
        let field = self
            .fields
            .iter()
            .find(|f| f.name == field_name)
            .ok_or_else(|| format!("Field {} not found", field_name))?;

        #[allow(unreachable_patterns)] // Match all pattern types
        match &field.field_type {
            TemplateFieldType::Text | TemplateFieldType::LongText => {
                if value.is_empty() && field.required {
                    return Err(format!("Field {} is required", field_name));
                }
                Ok(())
            }
            TemplateFieldType::Date => {
                // Basic ISO 8601 validation
                if value.len() != 10 || !value.contains('-') {
                    return Err(format!("Invalid date format: {}", value));
                }
                Ok(())
            }
            TemplateFieldType::Select(options) => {
                if !options.contains(&value.to_string()) {
                    return Err(format!(
                        "Invalid option: {}. Expected one of: {:?}",
                        value, options
                    ));
                }
                Ok(())
            }
            TemplateFieldType::MultiSelect(options) => {
                let selected: Vec<&str> = value.split(',').map(|s| s.trim()).collect();
                for s in selected {
                    if !options.contains(&s.to_string()) {
                        return Err(format!(
                            "Invalid option: {}. Expected one of: {:?}",
                            s, options
                        ));
                    }
                }
                Ok(())
            }
            TemplateFieldType::Number => {
                value
                    .parse::<f64>()
                    .map_err(|_| format!("Invalid number: {}", value))?;
                Ok(())
            }
            TemplateFieldType::Boolean => match value.to_lowercase().as_str() {
                "true" | "false" | "yes" | "no" | "1" | "0" => Ok(()),
                _ => Err(format!("Invalid boolean: {}", value)),
            },
        }
    }
}

/// Builder for creating templates
pub struct TemplateBuilder {
    id: String,
    name: String,
    description: String,
    category: String,
    frontmatter_template: HashMap<String, String>,
    fields: Vec<TemplateField>,
    content_template: String,
    example_output: String,
}

impl TemplateBuilder {
    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    pub fn category(mut self, cat: impl Into<String>) -> Self {
        self.category = cat.into();
        self
    }

    pub fn add_frontmatter(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.frontmatter_template.insert(key.into(), value.into());
        self
    }

    pub fn add_field(mut self, field: TemplateField) -> Self {
        self.fields.push(field);
        self
    }

    pub fn content_template(mut self, template: impl Into<String>) -> Self {
        self.content_template = template.into();
        self
    }

    pub fn example_output(mut self, example: impl Into<String>) -> Self {
        self.example_output = example.into();
        self
    }

    pub fn build(self) -> TemplateDefinition {
        TemplateDefinition {
            id: self.id,
            name: self.name,
            description: self.description,
            category: self.category,
            frontmatter_template: self.frontmatter_template,
            fields: self.fields,
            content_template: self.content_template,
            example_output: self.example_output,
        }
    }
}

/// Info about created note from template
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatedNoteInfo {
    pub path: String,
    pub title: String,
    pub template_id: String,
    pub content_preview: String,
}

/// Template engine for vault
pub struct TemplateEngine {
    pub manager: Arc<VaultManager>,
    templates: HashMap<String, TemplateDefinition>,
}

impl TemplateEngine {
    /// Create new template engine
    pub fn new(manager: Arc<VaultManager>) -> Self {
        Self {
            manager,
            templates: Self::default_templates(),
        }
    }

    /// Get predefined templates for common vault patterns
    fn default_templates() -> HashMap<String, TemplateDefinition> {
        let mut templates = HashMap::new();

        // Documentation template
        let doc_template = TemplateDefinition::builder("doc", "Documentation")
            .description("Standard documentation note")
            .category("documentation")
            .add_frontmatter("type", "documentation")
            .add_frontmatter("status", "draft")
            .add_field(TemplateField {
                name: "title".to_string(),
                description: "Note title".to_string(),
                field_type: TemplateFieldType::Text,
                required: true,
                default_value: None,
                example: Some("User Authentication".to_string()),
            })
            .add_field(TemplateField {
                name: "summary".to_string(),
                description: "Brief summary".to_string(),
                field_type: TemplateFieldType::Text,
                required: true,
                default_value: None,
                example: Some("Explains how users authenticate in the system".to_string()),
            })
            .add_field(TemplateField {
                name: "tags".to_string(),
                description: "Comma-separated tags".to_string(),
                field_type: TemplateFieldType::MultiSelect(vec![
                    "architecture".to_string(),
                    "security".to_string(),
                    "guide".to_string(),
                ]),
                required: false,
                default_value: None,
                example: None,
            })
            .content_template(
                "# {title}\n\n{summary}\n\n## Overview\n\n## Details\n\n## Links\n- Related: [[]]"
                    .to_string(),
            )
            .example_output(
                "# User Authentication\n\nExplains JWT-based auth...\n\n## Overview\n...",
            )
            .build();
        templates.insert("doc".to_string(), doc_template);

        // Task template
        let task_template = TemplateDefinition::builder("task", "Task")
            .description("Action item or task")
            .category("tasks")
            .add_frontmatter("type", "task")
            .add_frontmatter("status", "todo")
            .add_field(TemplateField {
                name: "title".to_string(),
                description: "Task title".to_string(),
                field_type: TemplateFieldType::Text,
                required: true,
                default_value: None,
                example: Some("Implement user registration".to_string()),
            })
            .add_field(TemplateField {
                name: "priority".to_string(),
                description: "Priority level".to_string(),
                field_type: TemplateFieldType::Select(vec![
                    "low".to_string(),
                    "medium".to_string(),
                    "high".to_string(),
                    "critical".to_string(),
                ]),
                required: true,
                default_value: Some("medium".to_string()),
                example: None,
            })
            .add_field(TemplateField {
                name: "due_date".to_string(),
                description: "Due date (YYYY-MM-DD)".to_string(),
                field_type: TemplateFieldType::Date,
                required: false,
                default_value: None,
                example: Some("2025-12-31".to_string()),
            })
            .content_template(
                "# {title}\n\n## Priority: {priority}\n\n## Description\n\n## Checklist\n- [ ] ",
            )
            .build();
        templates.insert("task".to_string(), task_template);

        // Research note template
        let research_template = TemplateDefinition::builder("research", "Research Note")
            .description("Research finding or investigation")
            .category("research")
            .add_frontmatter("type", "research")
            .add_field(TemplateField {
                name: "topic".to_string(),
                description: "Research topic".to_string(),
                field_type: TemplateFieldType::Text,
                required: true,
                default_value: None,
                example: Some("Rust async/await patterns".to_string()),
            })
            .add_field(TemplateField {
                name: "date_researched".to_string(),
                description: "Date of research (YYYY-MM-DD)".to_string(),
                field_type: TemplateFieldType::Date,
                required: true,
                default_value: None,
                example: None,
            })
            .content_template(
                "# {topic}\n\nResearched: {date_researched}\n\n## Key Findings\n\n## Sources\n\n## Related"
                    .to_string(),
            )
            .build();
        templates.insert("research".to_string(), research_template);

        templates
    }

    /// List all available templates
    pub fn list_templates(&self) -> Vec<TemplateDefinition> {
        self.templates.values().cloned().collect()
    }

    /// Get template by ID
    pub fn get_template(&self, id: &str) -> Option<TemplateDefinition> {
        self.templates.get(id).cloned()
    }

    /// Register custom template
    pub fn register_template(&mut self, template: TemplateDefinition) {
        self.templates.insert(template.id.clone(), template);
    }

    /// Create note from template (LLM fills in fields)
    pub async fn create_from_template(
        &self,
        template_id: &str,
        file_path: &str,
        field_values: HashMap<String, String>,
    ) -> crate::Result<CreatedNoteInfo> {
        let template = self.get_template(template_id).ok_or_else(|| {
            crate::Error::not_found(format!("Template {} not found", template_id))
        })?;

        // Validate all required fields
        for field in template.required_fields() {
            let value = field_values.get(&field.name).ok_or_else(|| {
                crate::Error::validation_error(format!("Missing required field: {}", field.name))
            })?;
            template
                .validate_field(&field.name, value)
                .map_err(crate::Error::validation_error)?;
        }

        // Build frontmatter
        let mut frontmatter = template.frontmatter_template.clone();
        frontmatter.insert("template".to_string(), template_id.to_string());
        if let Some(title) = field_values.get("title") {
            frontmatter.insert("title".to_string(), title.clone());
        }

        // Render content by substituting field values
        let mut content = template.content_template.clone();
        for (key, value) in &field_values {
            content = content.replace(&format!("{{{}}}", key), value);
        }

        // Create note in vault
        let mut frontmatter_yaml = String::from("---\n");
        for (key, value) in frontmatter {
            frontmatter_yaml.push_str(&format!("{}: {}\n", key, value));
        }
        frontmatter_yaml.push_str("---\n");

        let full_content = format!("{}{}", frontmatter_yaml, content);

        self.manager
            .write_file(Path::new(file_path), &full_content)
            .await?;

        Ok(CreatedNoteInfo {
            path: file_path.to_string(),
            title: field_values.get("title").cloned().unwrap_or_default(),
            template_id: template_id.to_string(),
            content_preview: content.lines().take(3).collect::<Vec<_>>().join("\n"),
        })
    }

    /// Find notes created from specific template
    pub async fn find_notes_from_template(&self, template_id: &str) -> crate::Result<Vec<String>> {
        let files = self.manager.scan_vault().await?;
        let mut results = Vec::new();

        for file in files {
            if let Ok(vault_file) = self.manager.parse_file(&file).await {
                let matches = vault_file
                    .frontmatter
                    .as_ref()
                    .and_then(|fm| fm.data.get("template"))
                    .and_then(|v| v.as_str())
                    .map(|t| t == template_id)
                    .unwrap_or(false);

                if matches {
                    results.push(file.to_string_lossy().to_string());
                }
            }
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_template_builder() {
        let template = TemplateDefinition::builder("test", "Test Template")
            .description("A test template")
            .category("test")
            .build();

        assert_eq!(template.id, "test");
        assert_eq!(template.name, "Test Template");
        assert_eq!(template.category, "test");
    }

    #[test]
    fn test_field_validation() {
        let template = TemplateDefinition::builder("test", "Test")
            .add_field(TemplateField {
                name: "status".to_string(),
                description: "Status".to_string(),
                field_type: TemplateFieldType::Select(vec![
                    "open".to_string(),
                    "closed".to_string(),
                ]),
                required: true,
                default_value: None,
                example: None,
            })
            .build();

        assert!(template.validate_field("status", "open").is_ok());
        assert!(template.validate_field("status", "invalid").is_err());
    }

    #[test]
    fn test_date_validation() {
        let template = TemplateDefinition::builder("test", "Test")
            .add_field(TemplateField {
                name: "date".to_string(),
                description: "Date".to_string(),
                field_type: TemplateFieldType::Date,
                required: true,
                default_value: None,
                example: None,
            })
            .build();

        assert!(template.validate_field("date", "2025-12-31").is_ok());
        assert!(template.validate_field("date", "invalid").is_err());
    }

    #[test]
    fn test_default_templates() {
        // Create a test manager with proper config
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let vault_path = temp_dir.path();

        let mut config = turbovault_core::ServerConfig::new();
        let vault_config = turbovault_core::VaultConfig::builder("test", vault_path)
            .build()
            .unwrap();
        config.vaults.push(vault_config);

        let manager = turbovault_vault::VaultManager::new(config).unwrap();
        let engine = TemplateEngine::new(Arc::new(manager));

        let templates = engine.list_templates();
        assert!(!templates.is_empty());
        assert!(engine.get_template("doc").is_some());
        assert!(engine.get_template("task").is_some());
        assert!(engine.get_template("research").is_some());
    }
}
