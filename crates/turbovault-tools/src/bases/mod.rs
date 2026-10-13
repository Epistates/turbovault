//! Obsidian Bases (`.base` files): a saved query over the vault's notes,
//! evaluated the way Obsidian's Bases core plugin does.
//!
//! Follows the format documented at <https://obsidian.md/help/bases/syntax>
//! and <https://obsidian.md/help/bases/functions>: a YAML file with
//! `filters` (an expression, or `and`/`or`/`not` lists of them), `formulas`
//! (named expressions, read as `formula.<name>`), `properties` (display
//! names), and `views`, each with a `type`, `name`, its own `filters`,
//! `order` (the columns), `sort`, `groupBy` and `limit`. Properties are read
//! as `note.status` or `status`, file facts as `file.name`, `file.path`,
//! `file.folder`, `file.ext`, `file.size`, `file.ctime`, `file.mtime`,
//! `file.tags`, `file.links`, `file.backlinks`, `file.embeds` and
//! `file.properties`, and the base's own file as `this.file`.
//!
//! This evaluates expressions directly rather than translating them to the
//! SQL engine: Bases semantics (methods on values, links that compare by the
//! note they resolve to, nested tags, duration arithmetic on dates, lambdas
//! in `filter`/`map`/`reduce`) have no faithful SQL form. A function or
//! method it doesn't evaluate is refused up front, by name, never guessed
//! at. What it evaluates is listed in [`expr::FUNCTIONS`] and
//! [`expr::METHODS`]; `summaries`, `groupOrder` and the HTML, image, icon,
//! `random`, `relative` and regular-expression functions are not.
//!
//! Rows are the vault's notes (Markdown). Obsidian also lists attachments
//! in a base; those aren't indexed here, so a base meant to list them
//! returns no rows for them.

pub mod eval;
pub mod expr;

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use chrono::NaiveDateTime;
use serde::Serialize;
use serde_json::Value as Json;
use turbovault_core::prelude::*;
use turbovault_core::{LinkType, VaultFile, property_tags};
use turbovault_vault::VaultManager;

use self::eval::{Env, FileInfo, Row, to_json, truthy};
use self::expr::Expr;

/// A filter: one expression, or every, any or none of several.
#[derive(Debug, Clone)]
pub enum Filter {
    Expr(String, Expr),
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Not(Vec<Filter>),
}

/// A property to sort or group by, and its direction.
#[derive(Debug, Clone)]
pub struct SortKey {
    pub property: String,
    pub expr: Expr,
    pub descending: bool,
}

/// One view of a base.
#[derive(Debug, Clone)]
pub struct View {
    pub kind: String,
    pub name: Option<String>,
    pub limit: Option<usize>,
    pub filters: Option<Filter>,
    /// The columns, as property ids, with their parsed expressions.
    pub order: Vec<(String, Expr)>,
    pub sort: Vec<SortKey>,
    pub group_by: Option<SortKey>,
}

/// A parsed `.base` file.
#[derive(Debug, Clone, Default)]
pub struct Base {
    pub filters: Option<Filter>,
    pub formulas: Vec<(String, Expr)>,
    /// Display names, by property id.
    pub display_names: BTreeMap<String, String>,
    pub views: Vec<View>,
}

/// Parse a `.base` file, refusing anything it can't evaluate faithfully.
pub fn parse_base(yaml: &str) -> Result<Base> {
    let document: Json = if yaml.trim().is_empty() {
        Json::Object(Default::default())
    } else {
        yaml_serde::from_str(yaml)
            .map_err(|error| Error::validation_error(format!("not a valid .base file: {error}")))?
    };
    let Json::Object(document) = document else {
        return Err(Error::validation_error(
            "not a valid .base file: the top level must be a mapping".to_string(),
        ));
    };
    let invalid = |message: String| Error::validation_error(message);

    let filters = document
        .get("filters")
        .filter(|f| !f.is_null())
        .map(|f| parse_filter(f, "filters"))
        .transpose()
        .map_err(invalid)?;

    let mut formulas = Vec::new();
    if let Some(Json::Object(map)) = document.get("formulas") {
        for (name, source) in map {
            let source = json_text(source)
                .ok_or_else(|| invalid(format!("formula {name:?} is not an expression")))?;
            formulas.push((name.clone(), compile(&source).map_err(invalid)?));
        }
    }

    let mut display_names = BTreeMap::new();
    if let Some(Json::Object(map)) = document.get("properties") {
        for (id, config) in map {
            if let Some(name) = config.get("displayName").and_then(Json::as_str) {
                display_names.insert(id.clone(), name.to_string());
            }
        }
    }

    let mut views = Vec::new();
    if let Some(list) = document.get("views") {
        let Json::Array(list) = list else {
            return Err(invalid("views must be a list".to_string()));
        };
        for (i, view) in list.iter().enumerate() {
            views.push(parse_view(view, i).map_err(invalid)?);
        }
    }

    for key in ["summaries"] {
        if document.get(key).is_some_and(|v| !v.is_null()) {
            // Summaries only aggregate what a view shows; rows are
            // unaffected, so they are ignored rather than refused.
            let _ = key;
        }
    }

    Ok(Base {
        filters,
        formulas,
        display_names,
        views,
    })
}

fn compile(source: &str) -> std::result::Result<Expr, String> {
    let parsed = expr::parse(source)?;
    expr::check(&parsed, source)?;
    Ok(parsed)
}

fn json_text(value: &Json) -> Option<String> {
    match value {
        Json::String(s) => Some(s.clone()),
        Json::Number(n) => Some(n.to_string()),
        Json::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn parse_filter(value: &Json, at: &str) -> std::result::Result<Filter, String> {
    match value {
        Json::String(_) | Json::Number(_) | Json::Bool(_) => {
            let source = json_text(value).unwrap_or_default();
            Ok(Filter::Expr(source.clone(), compile(&source)?))
        }
        Json::Object(map) => {
            let (key, list) = match map.iter().next() {
                Some(entry) if map.len() == 1 => entry,
                _ => {
                    return Err(format!("{at} must be an expression or one of and, or, not"));
                }
            };
            let Json::Array(items) = list else {
                return Err(format!("{at}.{key} must be a list"));
            };
            let items = items
                .iter()
                .enumerate()
                .map(|(i, item)| parse_filter(item, &format!("{at}.{key}[{i}]")))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            match key.as_str() {
                "and" => Ok(Filter::And(items)),
                "or" => Ok(Filter::Or(items)),
                "not" => Ok(Filter::Not(items)),
                other => Err(format!("{at} has {other:?}; expected and, or or not")),
            }
        }
        _ => Err(format!("{at} must be an expression or one of and, or, not")),
    }
}

fn parse_sort_key(value: &Json, at: &str) -> std::result::Result<SortKey, String> {
    let property = value
        .get("property")
        .and_then(Json::as_str)
        .ok_or_else(|| format!("{at} needs a property"))?
        .to_string();
    let descending = match value.get("direction").and_then(Json::as_str) {
        None => false,
        Some(d) if d.eq_ignore_ascii_case("asc") => false,
        Some(d) if d.eq_ignore_ascii_case("desc") => true,
        Some(other) => {
            return Err(format!(
                "{at} has direction {other:?}; expected ASC or DESC"
            ));
        }
    };
    Ok(SortKey {
        expr: compile(&property)?,
        property,
        descending,
    })
}

fn parse_view(value: &Json, index: usize) -> std::result::Result<View, String> {
    let at = format!("views[{index}]");
    let kind = value
        .get("type")
        .and_then(Json::as_str)
        .unwrap_or("table")
        .to_string();
    let name = value.get("name").and_then(Json::as_str).map(str::to_string);
    let limit = match value.get("limit") {
        None | Some(Json::Null) => None,
        Some(limit) => Some(
            limit
                .as_u64()
                .ok_or_else(|| format!("{at}.limit must be a whole number"))? as usize,
        ),
    };
    let filters = value
        .get("filters")
        .filter(|f| !f.is_null())
        .map(|f| parse_filter(f, &format!("{at}.filters")))
        .transpose()?;
    let order = match value.get("order") {
        Some(Json::Array(items)) => items
            .iter()
            .filter_map(Json::as_str)
            .map(|id| Ok((id.to_string(), compile(id)?)))
            .collect::<std::result::Result<Vec<_>, String>>()?,
        _ => Vec::new(),
    };
    let sort = match value.get("sort") {
        Some(Json::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(i, item)| parse_sort_key(item, &format!("{at}.sort[{i}]")))
            .collect::<std::result::Result<Vec<_>, _>>()?,
        _ => Vec::new(),
    };
    let group_by = value
        .get("groupBy")
        .filter(|g| !g.is_null())
        .map(|g| parse_sort_key(g, &format!("{at}.groupBy")))
        .transpose()?;
    Ok(View {
        kind,
        name,
        limit,
        filters,
        order,
        sort,
        group_by,
    })
}

/// One column of a result.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct BaseColumn {
    /// The property id, as the base names it (`file.name`, `status`,
    /// `formula.price`).
    pub id: String,
    /// Its display name.
    pub name: String,
}

/// One note in a result.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct BaseRow {
    pub path: String,
    /// The view's columns, by property id.
    pub values: serde_json::Map<String, Json>,
    /// The row's group, when the view groups.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<Json>,
}

/// A view of a base, evaluated.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct BaseResult {
    pub base: String,
    /// The view's name, if it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
    /// `table`, `cards`, `list`, or another view type, as the base says.
    pub view_type: String,
    /// Every view's name, for asking for another.
    pub views: Vec<String>,
    pub columns: Vec<BaseColumn>,
    pub rows: Vec<BaseRow>,
    /// The property the rows are grouped by, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_by: Option<String>,
    /// Notes an expression couldn't be evaluated on, and why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

const MAX_WARNINGS: usize = 20;

/// Evaluate the view named `view_name` (or the first view) of the base at
/// `base_path`. Rows come back sorted and cut to the view's limit.
pub async fn query_base(
    manager: &Arc<VaultManager>,
    base_path: &str,
    view_name: Option<&str>,
    now: NaiveDateTime,
) -> Result<BaseResult> {
    if !Path::new(base_path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("base"))
    {
        return Err(Error::invalid_path(format!(
            "{base_path} is not a base: base files end in .base"
        )));
    }
    let yaml = manager.read_file(Path::new(base_path)).await?;
    let base = parse_base(&yaml)?;

    let view_names: Vec<String> = base
        .views
        .iter()
        .enumerate()
        .map(|(i, v)| v.name.clone().unwrap_or_else(|| format!("View {}", i + 1)))
        .collect();
    let view = match view_name {
        Some(wanted) => {
            let index = view_names
                .iter()
                .position(|name| name == wanted)
                .ok_or_else(|| {
                    Error::validation_error(format!(
                        "{base_path} has no view named {wanted:?}; its views are {}",
                        if view_names.is_empty() {
                            "none".to_string()
                        } else {
                            view_names.join(", ")
                        }
                    ))
                })?;
            base.views[index].clone()
        }
        None => base.views.first().cloned().unwrap_or(View {
            kind: "table".to_string(),
            name: None,
            limit: None,
            filters: None,
            order: Vec::new(),
            sort: Vec::new(),
            group_by: None,
        }),
    };
    let columns: Vec<(String, Expr)> = if view.order.is_empty() {
        vec![(
            "file.name".to_string(),
            compile("file.name").map_err(Error::validation_error)?,
        )]
    } else {
        view.order.clone()
    };

    let notes = manager.vault_files_validated().await;
    let mut files: Vec<FileInfo> = notes.iter().map(|note| file_info(manager, note)).collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let base_full = manager.vault_path().join(base_path);
    let base_size = tokio::fs::metadata(&base_full)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    files.push(FileInfo {
        path: base_path.to_string(),
        full_path: base_full,
        size: base_size,
        ctime: None,
        mtime: None,
        properties: Default::default(),
        tags: Vec::new(),
        links: Vec::new(),
        embeds: Vec::new(),
    });
    let note_count = files.len() - 1;

    let graph = manager.link_graph_flushed().await;
    let graph = graph.read().await;
    let env = Env {
        files,
        graph: &graph,
        now,
        formulas: base.formulas.iter().cloned().collect::<HashMap<_, _>>(),
    };

    let mut warnings = Vec::new();
    let warn = |warnings: &mut Vec<String>, path: &str, error: String| {
        if warnings.len() < MAX_WARNINGS {
            warnings.push(format!("{path}: {error}"));
        }
    };

    struct Pending {
        index: usize,
        keys: Vec<eval::Value>,
        values: serde_json::Map<String, Json>,
        group: Option<Json>,
    }
    let mut pending = Vec::new();
    for index in 0..note_count {
        let path = env.files[index].path.clone();
        let mut row = Row::new(&env, index);
        let included = [&base.filters, &view.filters]
            .into_iter()
            .flatten()
            .try_fold(true, |keep, filter| {
                Ok::<bool, String>(keep && matches(&mut row, filter)?)
            });
        match included {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                warn(&mut warnings, &path, error);
                continue;
            }
        }
        let mut keys = Vec::new();
        for key in view.group_by.iter().chain(&view.sort) {
            keys.push(row.eval(&key.expr).unwrap_or_else(|error| {
                warn(&mut warnings, &path, error);
                eval::Value::Null
            }));
        }
        let mut values = serde_json::Map::new();
        for (id, expr) in &columns {
            let value = row.eval(expr).unwrap_or_else(|error| {
                warn(&mut warnings, &path, error);
                eval::Value::Null
            });
            values.insert(id.clone(), to_json(&value, &env));
        }
        let group = view.group_by.as_ref().map(|_| to_json(&keys[0], &env));
        pending.push(Pending {
            index,
            keys,
            values,
            group,
        });
    }

    let directions: Vec<bool> = view
        .group_by
        .iter()
        .chain(&view.sort)
        .map(|key| key.descending)
        .collect();
    let comparer = Row::new(&env, 0);
    pending.sort_by(|a, b| {
        for (i, descending) in directions.iter().enumerate() {
            let ordering = match (&a.keys[i], &b.keys[i]) {
                // Empty values stay last in either direction.
                (eval::Value::Null, eval::Value::Null) => Ordering::Equal,
                (eval::Value::Null, _) => Ordering::Greater,
                (_, eval::Value::Null) => Ordering::Less,
                (x, y) => {
                    let o = comparer.sort_order(x, y);
                    if *descending { o.reverse() } else { o }
                }
            };
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        env.files[a.index].path.cmp(&env.files[b.index].path)
    });
    if let Some(limit) = view.limit {
        pending.truncate(limit);
    }

    let rows = pending
        .into_iter()
        .map(|p| BaseRow {
            path: env.files[p.index].path.clone(),
            values: p.values,
            group: p.group,
        })
        .collect();
    let display = |id: &str| {
        base.display_names
            .get(id)
            .or_else(|| {
                id.strip_prefix("note.")
                    .and_then(|bare| base.display_names.get(bare))
            })
            .or_else(|| base.display_names.get(&format!("note.{id}")))
            .cloned()
            .unwrap_or_else(|| id.strip_prefix("note.").unwrap_or(id).to_string())
    };
    Ok(BaseResult {
        base: base_path.to_string(),
        view: view.name.clone(),
        view_type: view.kind.clone(),
        views: view_names,
        columns: columns
            .iter()
            .map(|(id, _)| BaseColumn {
                id: id.clone(),
                name: display(id),
            })
            .collect(),
        rows,
        group_by: view.group_by.as_ref().map(|key| key.property.clone()),
        warnings,
    })
}

fn matches(row: &mut Row<'_, '_>, filter: &Filter) -> std::result::Result<bool, String> {
    Ok(match filter {
        Filter::Expr(_, expr) => truthy(&row.eval(expr)?),
        Filter::And(items) => {
            for item in items {
                if !matches(row, item)? {
                    return Ok(false);
                }
            }
            true
        }
        Filter::Or(items) => {
            for item in items {
                if matches(row, item)? {
                    return Ok(true);
                }
            }
            false
        }
        Filter::Not(items) => {
            for item in items {
                if matches(row, item)? {
                    return Ok(false);
                }
            }
            true
        }
    })
}

/// What an expression can know about `note`.
fn file_info(manager: &VaultManager, note: &VaultFile) -> FileInfo {
    let properties: serde_json::Map<String, Json> = note
        .frontmatter
        .as_ref()
        .map(|fm| {
            fm.data
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default();
    let mut tags: Vec<String> = property_tags(properties.get("tags"))
        .into_iter()
        .chain(note.tags.iter().map(|tag| tag.name.clone()))
        .map(|tag| format!("#{}", tag.trim_start_matches('#')))
        .collect();
    tags.sort();
    tags.dedup();
    let link_pair = |link: &turbovault_core::Link| (link.target.clone(), link.display_text.clone());
    let internal = |link: &&turbovault_core::Link| {
        !matches!(link.type_, LinkType::ExternalLink | LinkType::Anchor)
    };
    let links = note
        .links
        .iter()
        .filter(internal)
        .filter(|l| l.type_ != LinkType::Embed)
        .map(link_pair)
        .collect();
    let embeds = note
        .links
        .iter()
        .filter(|l| l.type_ == LinkType::Embed)
        .map(link_pair)
        .collect();
    FileInfo {
        path: manager.relative_path(&note.path),
        full_path: note.path.clone(),
        size: note.metadata.size,
        ctime: timestamp(note.metadata.created_at),
        mtime: timestamp(note.metadata.modified_at),
        properties,
        tags,
        links,
        embeds,
    }
}

fn timestamp(seconds: f64) -> Option<NaiveDateTime> {
    if seconds <= 0.0 {
        return None;
    }
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt((seconds * 1000.0) as i64)
        .single()
        .map(|at| at.naive_local())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use turbovault_core::{ConfigProfile, VaultConfig};

    fn write(root: &Path, path: &str, content: &str) {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }

    async fn vault() -> (TempDir, Arc<VaultManager>) {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        write(
            root,
            "Books/Dune.md",
            "---\nauthor: Herbert\nprice: 12.5\nage: 5\nstatus: reading\nseries: \"[[Dune Saga]]\"\ntags:\n  - book\n  - book/scifi\n---\n# Dune\nSee [[Foundation]]. #favorite\n",
        );
        write(
            root,
            "Books/Foundation.md",
            "---\nauthor: Asimov\nprice: 8\nage: 2\nstatus: done\ntags: [book]\n---\n# Foundation\n",
        );
        write(
            root,
            "Notes/Idea.md",
            "---\nstatus: draft\n---\n# Idea\nAbout [[Dune]].\n",
        );
        write(root, "Dune Saga.md", "# Saga\n");
        let mut config = ConfigProfile::Development.create_config();
        config.vaults = vec![VaultConfig::builder("v", root).build().unwrap()];
        let manager = Arc::new(VaultManager::new(config).unwrap());
        manager.initialize().await.unwrap();
        (dir, manager)
    }

    fn now() -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 10, 13)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
    }

    async fn run(
        manager: &Arc<VaultManager>,
        root: &Path,
        yaml: &str,
        view: Option<&str>,
    ) -> Result<BaseResult> {
        write(root, "Library.base", yaml);
        query_base(manager, "Library.base", view, now()).await
    }

    fn paths(result: &BaseResult) -> Vec<&str> {
        result.rows.iter().map(|r| r.path.as_str()).collect()
    }

    #[tokio::test]
    async fn filters_formulas_sort_and_columns() {
        let (dir, manager) = vault().await;
        let result = run(
            &manager,
            dir.path(),
            r#"
filters:
  and:
    - file.hasTag("book")
    - 'status != "abandoned"'
formulas:
  ppu: "(price / age).toFixed(2)"
  label: 'if(status == "done", "finished", "open")'
properties:
  author:
    displayName: Author
views:
  - type: table
    name: Shelf
    order:
      - file.name
      - author
      - formula.ppu
      - formula.label
    sort:
      - property: price
        direction: DESC
"#,
            None,
        )
        .await
        .unwrap();
        assert_eq!(paths(&result), ["Books/Dune.md", "Books/Foundation.md"]);
        assert_eq!(result.view.as_deref(), Some("Shelf"));
        assert_eq!(result.columns[1].name, "Author");
        let dune = &result.rows[0].values;
        assert_eq!(dune["file.name"], "Dune.md");
        assert_eq!(dune["formula.ppu"], "2.50");
        assert_eq!(dune["formula.label"], "open");
        assert_eq!(result.rows[1].values["formula.label"], "finished");
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }

    #[tokio::test]
    async fn file_functions_links_and_not() {
        let (dir, manager) = vault().await;
        let links = run(
            &manager,
            dir.path(),
            "filters: 'file.hasLink(\"Dune\") || series == link(\"Dune Saga\")'\n",
            None,
        )
        .await
        .unwrap();
        assert_eq!(paths(&links), ["Books/Dune.md", "Notes/Idea.md"]);

        let negated = run(
            &manager,
            dir.path(),
            "filters:\n  not:\n    - file.inFolder(\"Books\")\n    - 'file.name == \"Dune Saga.md\"'\n",
            None,
        )
        .await
        .unwrap();
        assert_eq!(paths(&negated), ["Notes/Idea.md"]);

        let nested_tag = run(
            &manager,
            dir.path(),
            "filters: file.hasTag(\"book/scifi\")\n",
            None,
        )
        .await
        .unwrap();
        assert_eq!(paths(&nested_tag), ["Books/Dune.md"]);

        let backlinked = run(
            &manager,
            dir.path(),
            "filters: 'file.backlinks.length > 0'\n",
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            paths(&backlinked),
            ["Books/Dune.md", "Books/Foundation.md", "Dune Saga.md"]
        );
    }

    #[tokio::test]
    async fn views_group_limit_and_lambdas() {
        let (dir, manager) = vault().await;
        let yaml = r##"
views:
  - type: table
    name: All
  - type: cards
    name: By status
    limit: 2
    groupBy:
      property: status
      direction: ASC
    filters: 'file.tags.filter(value.startsWith("#book")).length > 0 || status == "draft"'
    order: [file.basename, status]
"##;
        let all = run(&manager, dir.path(), yaml, None).await.unwrap();
        assert_eq!(all.rows.len(), 4);
        assert_eq!(all.views, ["All", "By status"]);
        let grouped = run(&manager, dir.path(), yaml, Some("By status"))
            .await
            .unwrap();
        assert_eq!(grouped.view_type, "cards");
        assert_eq!(grouped.group_by.as_deref(), Some("status"));
        assert_eq!(paths(&grouped), ["Books/Foundation.md", "Notes/Idea.md"]);
        assert_eq!(grouped.rows[0].group, Some(Json::from("done")));

        let error = run(&manager, dir.path(), yaml, Some("Missing"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("All, By status"), "{error}");
    }

    #[tokio::test]
    async fn dates_and_durations() {
        let (dir, manager) = vault().await;
        let result = run(
            &manager,
            dir.path(),
            "filters: 'file.mtime > now() - \"1 day\" && date(\"2026-10-01\") + \"1M\" == date(\"2026-11-01\")'\n",
            None,
        )
        .await
        .unwrap();
        // The notes were all written moments ago, relative to the real
        // clock; `now()` is pinned to 2026-10-13, so compare against that.
        let real_now = chrono::Local::now().naive_local();
        let expected = if real_now > now() - chrono::Duration::days(1) {
            4
        } else {
            0
        };
        assert_eq!(result.rows.len(), expected);
    }

    #[tokio::test]
    async fn unsupported_and_broken_bases_are_refused_by_name() {
        let (dir, manager) = vault().await;
        for (yaml, expected) in [
            ("filters: 'file.name.matches(\"x\")'\n", "matches()"),
            ("formulas:\n  pic: 'image(cover)'\n", "image()"),
            ("filters: 'status =='\n", "ends too soon"),
            ("filters:\n  xor: []\n", "xor"),
            ("views: 3\n", "views must be a list"),
        ] {
            let error = run(&manager, dir.path(), yaml, None).await.unwrap_err();
            assert!(error.to_string().contains(expected), "{yaml}: {error}");
        }
        let error = query_base(&manager, "Books/Dune.md", None, now())
            .await
            .unwrap_err();
        assert!(error.to_string().contains(".base"), "{error}");
    }

    #[tokio::test]
    async fn a_type_error_on_one_note_is_a_warning() {
        let (dir, manager) = vault().await;
        let result = run(
            &manager,
            dir.path(),
            "filters: 'author.lower() == \"asimov\"'\n",
            None,
        )
        .await
        .unwrap();
        assert_eq!(paths(&result), ["Books/Foundation.md"]);
        assert!(result.warnings.is_empty());

        let result = run(
            &manager,
            dir.path(),
            "filters: 'price.lower() == \"x\"'\n",
            None,
        )
        .await
        .unwrap();
        assert!(result.rows.is_empty());
        assert_eq!(result.warnings.len(), 2, "{:?}", result.warnings);
        assert!(result.warnings[0].contains("no method lower()"));
    }
}
