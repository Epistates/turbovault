//! Daily, weekly, monthly, quarterly and yearly notes, named and placed the
//! way the vault's own settings say.
//!
//! The settings come from the Periodic Notes community plugin
//! (`.obsidian/plugins/periodic-notes/data.json`) when it's enabled and the
//! period is turned on there, and otherwise, for daily notes, from the core
//! Daily notes plugin (`.obsidian/daily-notes.json`). Each holds a `folder`,
//! a moment.js `format` for the note's name (which may include `/` to file
//! notes into subfolders) and a `template` path. A period with no settings
//! uses the plugins' defaults: `YYYY-MM-DD`, `gggg-[W]ww`, `YYYY-MM`,
//! `YYYY-[Q]Q` and `YYYY`, in the vault root, with no template.
//!
//! These files live under `.obsidian`, which no note tool reaches. They are
//! read here directly, for their settings only, and never written.
//!
//! A new note's template is filled the way Obsidian fills it: `{{title}}`,
//! `{{date}}` and `{{time}}` (in the Templates plugin's date and time
//! formats, `YYYY-MM-DD` and `HH:mm` unless `.obsidian/templates.json` says
//! otherwise), `{{date:FORMAT}}` and `{{time:FORMAT}}`, and the Periodic
//! Notes additions: `{{yesterday}}` and `{{tomorrow}}` in a daily note, and
//! `{{sunday:FORMAT}}` through `{{saturday:FORMAT}}` in a weekly one.

use std::path::Path;
use std::sync::Arc;

use chrono::{Datelike, Days, NaiveDate, NaiveDateTime, NaiveTime};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use turbovault_core::prelude::*;
use turbovault_core::Precondition;
use turbovault_vault::VaultManager;

use crate::moment_format::{format, start_of_locale_week, uses_iso_weeks};

/// A kind of periodic note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Period {
    Daily,
    Weekly,
    Monthly,
    Quarterly,
    Yearly,
}

impl Period {
    fn key(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
            Self::Quarterly => "quarterly",
            Self::Yearly => "yearly",
        }
    }

    fn default_format(self) -> &'static str {
        match self {
            Self::Daily => "YYYY-MM-DD",
            Self::Weekly => "gggg-[W]ww",
            Self::Monthly => "YYYY-MM",
            Self::Quarterly => "YYYY-[Q]Q",
            Self::Yearly => "YYYY",
        }
    }
}

/// Where a period's notes go and how they're named, and which settings
/// said so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct PeriodSettings {
    pub folder: String,
    pub format: String,
    /// The template note, vault-relative with its extension, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// `periodic-notes`, `daily-notes` or `default`.
    pub source: &'static str,
}

/// A periodic note, found or created.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct PeriodicNote {
    pub period: Period,
    /// The first day of the period, `YYYY-MM-DD`.
    pub date: String,
    /// Where the note is, or would be, vault-relative.
    pub path: String,
    pub exists: bool,
    /// Whether this call created it.
    pub created: bool,
    pub settings: PeriodSettings,
    /// The note's markdown, when it exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Its hash, for a later write's `expected_hash`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

/// The settings for `period` in the vault at `root`.
pub fn settings(root: &Path, period: Period) -> Result<PeriodSettings> {
    let config = root.join(".obsidian");
    let plugin_enabled = read_json(&config.join("community-plugins.json"))?
        .and_then(|value| value.as_array().cloned())
        .is_some_and(|plugins| plugins.iter().any(|p| p == "periodic-notes"));
    if plugin_enabled
        && let Some(data) = read_json(&config.join("plugins/periodic-notes/data.json"))?
        && let Some(entry) = data.get(period.key())
        && entry.get("enabled").and_then(Value::as_bool).unwrap_or(false)
    {
        return Ok(from_entry(entry, period, "periodic-notes"));
    }
    if period == Period::Daily
        && let Some(entry) = read_json(&config.join("daily-notes.json"))?
    {
        return Ok(from_entry(&entry, period, "daily-notes"));
    }
    Ok(PeriodSettings {
        folder: String::new(),
        format: period.default_format().to_string(),
        template: None,
        source: "default",
    })
}

fn from_entry(entry: &Value, period: Period, source: &'static str) -> PeriodSettings {
    let text = |key: &str| {
        entry
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    PeriodSettings {
        folder: text("folder")
            .map(|folder| folder.trim_matches('/').to_string())
            .unwrap_or_default(),
        format: text("format").unwrap_or_else(|| period.default_format().to_string()),
        template: text("template").map(|template| {
            let template = template.trim_start_matches('/');
            if Path::new(template).extension().is_some() {
                template.to_string()
            } else {
                format!("{template}.md")
            }
        }),
        source,
    }
}

/// A JSON settings file, or `None` when there is none. One that exists but
/// isn't JSON is an error: guessing at defaults would put notes in the
/// wrong place.
fn read_json(path: &Path) -> Result<Option<Value>> {
    const MAX_SETTINGS_BYTES: u64 = 1024 * 1024;
    let Ok(metadata) = std::fs::metadata(path) else {
        return Ok(None);
    };
    if metadata.len() > MAX_SETTINGS_BYTES {
        return Err(Error::config_error(format!(
            "{} is larger than a settings file should be",
            display_settings(path)
        )));
    }
    let text = std::fs::read_to_string(path).map_err(|error| {
        Error::config_error(format!("cannot read {}: {error}", display_settings(path)))
    })?;
    if text.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&text).map(Some).map_err(|error| {
        Error::config_error(format!("{} is not valid JSON: {error}", display_settings(path)))
    })
}

/// A settings path from `.obsidian` on, never the absolute path.
fn display_settings(path: &Path) -> String {
    let rendered = turbovault_core::path_to_slash(path);
    match rendered.find(".obsidian/") {
        Some(start) => rendered[start..].to_string(),
        None => rendered,
    }
}

/// The first day of the `period` holding `date`. A week starts on Sunday,
/// as in Obsidian's default locale, unless the name format numbers weeks
/// the ISO way, whose weeks start on Monday.
pub fn period_start(period: Period, date: NaiveDate, format: &str) -> NaiveDate {
    match period {
        Period::Daily => date,
        Period::Weekly if uses_iso_weeks(format) => {
            date - Days::new(u64::from(date.weekday().num_days_from_monday()))
        }
        Period::Weekly => start_of_locale_week(date),
        Period::Monthly => date.with_day(1).unwrap_or(date),
        Period::Quarterly => {
            NaiveDate::from_ymd_opt(date.year(), date.month0() / 3 * 3 + 1, 1).unwrap_or(date)
        }
        Period::Yearly => NaiveDate::from_ymd_opt(date.year(), 1, 1).unwrap_or(date),
    }
}

/// The vault-relative path of the `period` note for `start`.
pub fn note_path(settings: &PeriodSettings, start: NaiveDate) -> Result<String> {
    let name = format(start.and_time(NaiveTime::MIN), &settings.format)
        .map_err(Error::validation_error)?;
    let name = name.trim_matches('/');
    if name.is_empty() {
        return Err(Error::validation_error(format!(
            "the note name format {:?} produces an empty name",
            settings.format
        )));
    }
    Ok(if settings.folder.is_empty() {
        format!("{name}.md")
    } else {
        format!("{}/{name}.md", settings.folder)
    })
}

/// The `period` note holding `date`: read if it exists, and created from
/// its template when `create` is set and it doesn't.
pub async fn get_periodic_note(
    manager: &Arc<VaultManager>,
    period: Period,
    date: NaiveDate,
    now: NaiveDateTime,
    create: bool,
    message: &str,
) -> Result<PeriodicNote> {
    let root = manager.vault_path().clone();
    let settings = settings(&root, period)?;
    let start = period_start(period, date, &settings.format);
    let path = note_path(&settings, start)?;

    let existing = match manager.read_file(Path::new(&path)).await {
        Ok(content) => Some(content),
        Err(Error::FileNotFound { .. }) => None,
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let mut note = PeriodicNote {
        period,
        date: start.format("%Y-%m-%d").to_string(),
        path: path.clone(),
        exists: existing.is_some(),
        created: false,
        settings,
        content: None,
        hash: None,
    };
    let content = match existing {
        Some(content) => content,
        None if create => {
            let content = match &note.settings.template {
                Some(template) => {
                    let template_text = manager
                        .read_file(Path::new(template))
                        .await
                        .map_err(|error| match error {
                            Error::FileNotFound { .. } => Error::not_found(format!(
                                "template {template} (set for {} notes in Obsidian's settings)",
                                period.key()
                            )),
                            other => other,
                        })?;
                    let time_settings = template_formats(&root)?;
                    fill_template(
                        &template_text,
                        period,
                        start,
                        now,
                        &note.path,
                        &note.settings.format,
                        &time_settings,
                    )?
                }
                None => String::new(),
            };
            manager
                .write_file(Path::new(&path), &content, Precondition::ExpectAbsent, message)
                .await?;
            note.exists = true;
            note.created = true;
            content
        }
        None => return Ok(note),
    };
    note.hash = Some(manager.hash_bytes(content.as_bytes())?);
    note.content = Some(content);
    Ok(note)
}

/// The Templates plugin's `{{date}}` and `{{time}}` formats.
fn template_formats(root: &Path) -> Result<(String, String)> {
    let settings = read_json(&root.join(".obsidian/templates.json"))?;
    let text = |key: &str, default: &str| {
        settings
            .as_ref()
            .and_then(|s| s.get(key))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(default)
            .to_string()
    };
    Ok((text("dateFormat", "YYYY-MM-DD"), text("timeFormat", "HH:mm")))
}

/// `template` with its variables filled for the note at `path`. A variable
/// this doesn't know is left as written, as Obsidian leaves it.
fn fill_template(
    template: &str,
    period: Period,
    start: NaiveDate,
    now: NaiveDateTime,
    path: &str,
    note_format: &str,
    (date_format, time_format): &(String, String),
) -> Result<String> {
    let at = start.and_time(now.time());
    let title = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".md");
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find("}}") else {
            out.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let inner = &rest[open + 2..open + close];
        let (name, custom) = match inner.split_once(':') {
            Some((name, fmt)) => (name.trim(), Some(fmt)),
            None => (inner.trim(), None),
        };
        let filled = match (name.to_lowercase().as_str(), period) {
            ("title", _) => Some(title.to_string()),
            ("date", _) => Some(format(at, custom.unwrap_or(date_format)).map_err(Error::validation_error)?),
            ("time", _) => Some(
                format(now, custom.unwrap_or(time_format)).map_err(Error::validation_error)?,
            ),
            ("yesterday", Period::Daily) => Some(
                format(at - Days::new(1), custom.unwrap_or(note_format))
                    .map_err(Error::validation_error)?,
            ),
            ("tomorrow", Period::Daily) => Some(
                format(at + Days::new(1), custom.unwrap_or(note_format))
                    .map_err(Error::validation_error)?,
            ),
            (day, Period::Weekly) => match weekday_offset(day) {
                Some(offset) => {
                    let sunday = start_of_locale_week(start);
                    Some(
                        format(
                            (sunday + Days::new(offset)).and_time(now.time()),
                            custom.unwrap_or(note_format),
                        )
                        .map_err(Error::validation_error)?,
                    )
                }
                None => None,
            },
            _ => None,
        };
        match filled {
            Some(filled) => out.push_str(&filled),
            None => out.push_str(&rest[open..open + close + 2]),
        }
        rest = &rest[open + close + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

fn weekday_offset(name: &str) -> Option<u64> {
    [
        "sunday",
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
    ]
    .iter()
    .position(|day| *day == name)
    .map(|i| i as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use turbovault_core::{ConfigProfile, VaultConfig};

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn write(root: &Path, path: &str, content: &str) {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }

    async fn manager(root: &Path) -> Arc<VaultManager> {
        let mut config = ConfigProfile::Development.create_config();
        config.vaults = vec![VaultConfig::builder("v", root).build().unwrap()];
        let manager = Arc::new(VaultManager::new(config).unwrap());
        manager.initialize().await.unwrap();
        manager
    }

    #[test]
    fn defaults_without_settings() {
        let dir = TempDir::new().unwrap();
        let settings = settings(dir.path(), Period::Weekly).unwrap();
        assert_eq!(settings.source, "default");
        let start = period_start(Period::Weekly, day(2026, 10, 15), &settings.format);
        assert_eq!(start, day(2026, 10, 11));
        assert_eq!(note_path(&settings, start).unwrap(), "2026-W42.md");
        let quarter = period_start(Period::Quarterly, day(2026, 11, 30), "YYYY-[Q]Q");
        assert_eq!(quarter, day(2026, 10, 1));
    }

    #[test]
    fn core_daily_notes_settings() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            ".obsidian/daily-notes.json",
            r#"{"folder":"Journal/","format":"YYYY/MM/YYYY-MM-DD","template":"Templates/Daily"}"#,
        );
        let settings = settings(dir.path(), Period::Daily).unwrap();
        assert_eq!(settings.source, "daily-notes");
        assert_eq!(settings.template.as_deref(), Some("Templates/Daily.md"));
        assert_eq!(
            note_path(&settings, day(2026, 10, 13)).unwrap(),
            "Journal/2026/10/2026-10-13.md"
        );
        // Monthly notes have no core settings.
        assert_eq!(settings_source(dir.path(), Period::Monthly), "default");
    }

    fn settings_source(root: &Path, period: Period) -> &'static str {
        settings(root, period).unwrap().source
    }

    #[test]
    fn periodic_notes_plugin_wins_when_enabled() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), ".obsidian/daily-notes.json", r#"{"folder":"Core"}"#);
        write(
            dir.path(),
            ".obsidian/plugins/periodic-notes/data.json",
            r#"{"daily":{"enabled":true,"folder":"Plugin","format":"","template":""},
                "weekly":{"enabled":false,"folder":"Weeks","format":"GGGG-[W]WW"}}"#,
        );
        assert_eq!(settings_source(dir.path(), Period::Daily), "daily-notes");
        write(
            dir.path(),
            ".obsidian/community-plugins.json",
            r#"["periodic-notes"]"#,
        );
        let daily = settings(dir.path(), Period::Daily).unwrap();
        assert_eq!((daily.source, daily.folder.as_str()), ("periodic-notes", "Plugin"));
        assert_eq!(daily.format, "YYYY-MM-DD");
        // A period the plugin has turned off falls back to the defaults.
        assert_eq!(settings_source(dir.path(), Period::Weekly), "default");
    }

    #[test]
    fn broken_settings_are_an_error() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), ".obsidian/daily-notes.json", "{not json");
        let error = settings(dir.path(), Period::Daily).unwrap_err();
        assert!(error.to_string().contains(".obsidian/daily-notes.json"), "{error}");
    }

    #[test]
    fn templates_fill_obsidian_variables() {
        let now = day(2026, 10, 13).and_hms_opt(9, 5, 0).unwrap();
        let formats = ("YYYY-MM-DD".to_string(), "HH:mm".to_string());
        let daily = fill_template(
            "# {{title}}\n{{date:dddd}} at {{time}}\n<< {{yesterday}} | {{tomorrow}} >> {{unknown}}",
            Period::Daily,
            day(2026, 10, 13),
            now,
            "Journal/2026-10-13.md",
            "YYYY-MM-DD",
            &formats,
        )
        .unwrap();
        assert_eq!(
            daily,
            "# 2026-10-13\nTuesday at 09:05\n<< 2026-10-12 | 2026-10-14 >> {{unknown}}"
        );
        let weekly = fill_template(
            "{{monday:MMM D}} to {{sunday:MMM D}}",
            Period::Weekly,
            day(2026, 10, 11),
            now,
            "2026-W42.md",
            "gggg-[W]ww",
            &formats,
        )
        .unwrap();
        assert_eq!(weekly, "Oct 12 to Oct 11");
    }

    #[tokio::test]
    async fn creates_from_the_template_once() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            ".obsidian/daily-notes.json",
            r#"{"folder":"Daily","template":"Templates/Day"}"#,
        );
        write(dir.path(), "Templates/Day.md", "# {{title}}\n\n- [ ] plan\n");
        let manager = manager(dir.path()).await;
        let now = day(2026, 10, 13).and_hms_opt(8, 0, 0).unwrap();

        let missing = get_periodic_note(&manager, Period::Daily, day(2026, 10, 13), now, false, "m")
            .await
            .unwrap();
        assert!(!missing.exists && missing.content.is_none());
        assert_eq!(missing.path, "Daily/2026-10-13.md");

        let created = get_periodic_note(&manager, Period::Daily, day(2026, 10, 13), now, true, "m")
            .await
            .unwrap();
        assert!(created.created && created.exists);
        assert_eq!(created.content.as_deref(), Some("# 2026-10-13\n\n- [ ] plan\n"));

        let again = get_periodic_note(&manager, Period::Daily, day(2026, 10, 13), now, true, "m")
            .await
            .unwrap();
        assert!(again.exists && !again.created);
        assert_eq!(again.hash, created.hash);
    }

    #[tokio::test]
    async fn a_missing_template_is_not_found() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            ".obsidian/daily-notes.json",
            r#"{"template":"Templates/Gone"}"#,
        );
        let manager = manager(dir.path()).await;
        let now = day(2026, 10, 13).and_hms_opt(8, 0, 0).unwrap();
        let error = get_periodic_note(&manager, Period::Daily, day(2026, 10, 13), now, true, "m")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Templates/Gone.md"), "{error}");
    }
}
