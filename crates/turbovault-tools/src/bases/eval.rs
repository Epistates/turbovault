//! Evaluating Bases expressions against one note at a time.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use chrono::{Datelike, Duration as ChronoDuration, Months, NaiveDate, NaiveDateTime, Timelike};
use turbovault_graph::LinkGraph;

use super::expr::{BinaryOp, Expr, UnaryOp};

/// A value an expression produces.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    Str(String),
    Date(NaiveDateTime),
    Duration(Duration),
    List(Vec<Value>),
    Object(BTreeMap<String, Value>),
    /// A link to a note, as written (`[[Note]]` in a property, or `link()`).
    Link {
        target: String,
        display: Option<String>,
    },
    /// A file: one of the query's notes, or the base itself.
    File(usize),
}

/// A span of time: whole months (which vary in length) and milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Duration {
    pub months: i64,
    pub millis: i64,
}

/// What an expression can know about one file.
#[derive(Debug, Clone)]
pub struct FileInfo {
    /// Vault-relative path.
    pub path: String,
    pub full_path: PathBuf,
    pub size: u64,
    pub ctime: Option<NaiveDateTime>,
    pub mtime: Option<NaiveDateTime>,
    pub properties: serde_json::Map<String, serde_json::Value>,
    /// Every tag, with its `#`.
    pub tags: Vec<String>,
    /// Outgoing links, as written: target and display text.
    pub links: Vec<(String, Option<String>)>,
    pub embeds: Vec<(String, Option<String>)>,
}

impl FileInfo {
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    pub fn basename(&self) -> &str {
        let name = self.name();
        name.rsplit_once('.').map_or(name, |(stem, _)| stem)
    }

    pub fn ext(&self) -> &str {
        self.name().rsplit_once('.').map_or("", |(_, ext)| ext)
    }

    /// The folder holding the file, `/` for the vault root, as Obsidian
    /// names it.
    pub fn folder(&self) -> &str {
        self.path.rsplit_once('/').map_or("/", |(folder, _)| folder)
    }
}

/// Everything a query shares across its rows.
pub struct Env<'a> {
    /// The notes, then the base file itself as the last entry.
    pub files: Vec<FileInfo>,
    pub graph: &'a LinkGraph,
    pub now: NaiveDateTime,
    pub formulas: HashMap<String, Expr>,
}

impl Env<'_> {
    fn base(&self) -> usize {
        self.files.len() - 1
    }

    /// The file a value names: itself, a link's target, or a path or note
    /// name resolved the way a link would be.
    fn file_of(&self, value: &Value) -> Option<usize> {
        let target = match value {
            Value::File(index) => return Some(*index),
            Value::Link { target, .. } => target.clone(),
            Value::Str(text) => strip_brackets(text).to_string(),
            _ => return None,
        };
        let resolved = self.graph.resolve(&target)?;
        self.files
            .iter()
            .position(|file| &file.full_path == resolved)
    }
}

/// One row's evaluation: the note, lambda variables, and the formulas
/// already computed for it.
pub struct Row<'e, 'a> {
    env: &'e Env<'a>,
    row: usize,
    vars: Vec<(String, Value)>,
    formulas: HashMap<String, Value>,
    in_progress: Vec<String>,
}

type EvalResult = Result<Value, String>;

impl<'e, 'a> Row<'e, 'a> {
    pub fn new(env: &'e Env<'a>, row: usize) -> Self {
        Self {
            env,
            row,
            vars: Vec::new(),
            formulas: HashMap::new(),
            in_progress: Vec::new(),
        }
    }

    fn file(&self) -> &FileInfo {
        &self.env.files[self.row]
    }

    pub fn eval(&mut self, expr: &Expr) -> EvalResult {
        match expr {
            Expr::Number(n) => Ok(Value::Number(*n)),
            Expr::Str(s) => Ok(Value::Str(s.clone())),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Null => Ok(Value::Null),
            Expr::Ident(name) => self.ident(name),
            Expr::Member(target, name) => match target.as_ref() {
                Expr::Ident(ns) if ns == "formula" && !self.has_var("formula") => {
                    self.formula(name)
                }
                Expr::Ident(ns) if ns == "note" && !self.has_var("note") => {
                    Ok(property(&self.file().properties, name))
                }
                Expr::Ident(ns) if ns == "this" && name == "file" => {
                    Ok(Value::File(self.env.base()))
                }
                _ => {
                    let value = self.eval(target)?;
                    self.member(&value, name)
                }
            },
            Expr::Index(target, index) => {
                let value = self.eval(target)?;
                let index = self.eval(index)?;
                Ok(match (&value, &index) {
                    (Value::List(items), Value::Number(n)) if *n >= 0.0 => {
                        items.get(*n as usize).cloned().unwrap_or(Value::Null)
                    }
                    (Value::Object(map), Value::Str(key)) => {
                        map.get(key).cloned().unwrap_or(Value::Null)
                    }
                    (Value::Str(s), Value::Number(n)) if *n >= 0.0 => s
                        .chars()
                        .nth(*n as usize)
                        .map_or(Value::Null, |c| Value::Str(c.to_string())),
                    (Value::Null, _) => Value::Null,
                    _ => {
                        return Err(format!(
                            "can't index {} with {}",
                            type_name(&value),
                            type_name(&index)
                        ));
                    }
                })
            }
            Expr::Call { target, name, args } => match target {
                None => self.function(name, args),
                Some(target) => {
                    let receiver = self.eval(target)?;
                    self.method(&receiver, name, args)
                }
            },
            Expr::Unary(op, inner) => {
                let value = self.eval(inner)?;
                match op {
                    UnaryOp::Not => Ok(Value::Bool(!truthy(&value))),
                    UnaryOp::Neg => match value {
                        Value::Number(n) => Ok(Value::Number(-n)),
                        Value::Duration(d) => Ok(Value::Duration(Duration {
                            months: -d.months,
                            millis: -d.millis,
                        })),
                        other => Err(format!("can't negate {}", type_name(&other))),
                    },
                }
            }
            Expr::Binary(op, left, right) => self.binary(*op, left, right),
        }
    }

    fn has_var(&self, name: &str) -> bool {
        self.vars.iter().any(|(var, _)| var == name)
    }

    fn ident(&mut self, name: &str) -> EvalResult {
        if let Some((_, value)) = self.vars.iter().rev().find(|(var, _)| var == name) {
            return Ok(value.clone());
        }
        Ok(match name {
            "file" => Value::File(self.row),
            "note" => Value::Object(
                self.file()
                    .properties
                    .iter()
                    .map(|(k, v)| (k.clone(), from_json(v)))
                    .collect(),
            ),
            "this" => Value::Object(BTreeMap::from([(
                "file".to_string(),
                Value::File(self.env.base()),
            )])),
            "formula" => {
                return Err("formula needs a name: formula.<name>".to_string());
            }
            _ => property(&self.file().properties, name),
        })
    }

    fn formula(&mut self, name: &str) -> EvalResult {
        if let Some(value) = self.formulas.get(name) {
            return Ok(value.clone());
        }
        let expr = self
            .env
            .formulas
            .get(name)
            .ok_or_else(|| format!("there is no formula named {name:?}"))?;
        if self.in_progress.iter().any(|f| f == name) {
            return Err(format!("formula {name:?} depends on itself"));
        }
        self.in_progress.push(name.to_string());
        // Formulas see the note's own properties, not a caller's lambda
        // variables.
        let vars = std::mem::take(&mut self.vars);
        let result = self.eval(expr);
        self.vars = vars;
        self.in_progress.pop();
        let value = result?;
        self.formulas.insert(name.to_string(), value.clone());
        Ok(value)
    }

    fn member(&mut self, value: &Value, name: &str) -> EvalResult {
        Ok(match value {
            Value::Null => Value::Null,
            Value::File(index) => {
                let file = &self.env.files[*index];
                match name {
                    "name" => Value::Str(file.name().to_string()),
                    "basename" => Value::Str(file.basename().to_string()),
                    "path" => Value::Str(file.path.clone()),
                    "folder" => Value::Str(file.folder().to_string()),
                    "ext" => Value::Str(file.ext().to_string()),
                    "size" => Value::Number(file.size as f64),
                    "ctime" => file.ctime.map_or(Value::Null, Value::Date),
                    "mtime" => file.mtime.map_or(Value::Null, Value::Date),
                    "properties" => Value::Object(
                        file.properties
                            .iter()
                            .map(|(k, v)| (k.clone(), from_json(v)))
                            .collect(),
                    ),
                    "tags" => Value::List(file.tags.iter().cloned().map(Value::Str).collect()),
                    "links" => links(&file.links),
                    "embeds" => links(&file.embeds),
                    "backlinks" => {
                        let mut sources: Vec<String> = self
                            .env
                            .graph
                            .backlink_sources(&file.full_path)
                            .into_iter()
                            .filter_map(|path| {
                                self.env
                                    .files
                                    .iter()
                                    .find(|f| &f.full_path == path)
                                    .map(|f| f.path.clone())
                            })
                            .collect();
                        sources.sort();
                        Value::List(
                            sources
                                .into_iter()
                                .map(|target| Value::Link {
                                    target,
                                    display: None,
                                })
                                .collect(),
                        )
                    }
                    _ => return Err(format!("a file has no property {name:?}")),
                }
            }
            Value::Str(s) if name == "length" => Value::Number(s.chars().count() as f64),
            Value::List(items) if name == "length" => Value::Number(items.len() as f64),
            Value::Date(at) => Value::Number(match name {
                "year" => f64::from(at.year()),
                "month" => f64::from(at.month()),
                "day" => f64::from(at.day()),
                "hour" => f64::from(at.hour()),
                "minute" => f64::from(at.minute()),
                "second" => f64::from(at.second()),
                "millisecond" => f64::from(at.and_utc().timestamp_subsec_millis()),
                _ => return Err(format!("a date has no field {name:?}")),
            }),
            Value::Object(map) => map.get(name).cloned().unwrap_or(Value::Null),
            other => return Err(format!("{} has no field {name:?}", type_name(other))),
        })
    }

    fn args(&mut self, args: &[Expr]) -> Result<Vec<Value>, String> {
        args.iter().map(|arg| self.eval(arg)).collect()
    }

    fn function(&mut self, name: &str, args: &[Expr]) -> EvalResult {
        if name == "if" {
            let condition = self.eval(arg(args, 0, "if")?)?;
            return if truthy(&condition) {
                self.eval(arg(args, 1, "if")?)
            } else {
                args.get(2).map_or(Ok(Value::Null), |e| self.eval(e))
            };
        }
        let values = self.args(args)?;
        Ok(match name {
            "now" => Value::Date(self.env.now),
            "today" => Value::Date(self.env.now.date().and_time(chrono::NaiveTime::MIN)),
            "date" => match values.first() {
                Some(Value::Date(at)) => Value::Date(*at),
                Some(Value::Str(s)) => {
                    Value::Date(parse_date(s).ok_or_else(|| format!("date({s:?}) is not a date"))?)
                }
                _ => return Err("date() takes a date string".to_string()),
            },
            "duration" => match values.first() {
                Some(Value::Str(s)) => Value::Duration(
                    parse_duration(s)
                        .ok_or_else(|| format!("duration({s:?}) is not a duration"))?,
                ),
                Some(Value::Duration(d)) => Value::Duration(*d),
                _ => return Err("duration() takes a string such as \"1 day\"".to_string()),
            },
            "list" => match values.into_iter().next() {
                Some(Value::List(items)) => Value::List(items),
                Some(Value::Null) | None => Value::List(Vec::new()),
                Some(other) => Value::List(vec![other]),
            },
            "number" => match values.first() {
                Some(Value::Number(n)) => Value::Number(*n),
                Some(Value::Bool(b)) => Value::Number(if *b { 1.0 } else { 0.0 }),
                Some(Value::Str(s)) => s.trim().parse().map_or(Value::Null, Value::Number),
                Some(Value::Date(at)) => Value::Number(at.and_utc().timestamp_millis() as f64),
                _ => Value::Null,
            },
            "min" | "max" => {
                let numbers = flatten(values)
                    .into_iter()
                    .map(|v| match v {
                        Value::Number(n) => Ok(n),
                        other => Err(format!("{name}() takes numbers, not {}", type_name(&other))),
                    })
                    .collect::<Result<Vec<f64>, String>>()?;
                let pick = if name == "min" { f64::min } else { f64::max };
                numbers
                    .into_iter()
                    .reduce(pick)
                    .map_or(Value::Null, Value::Number)
            }
            "link" => {
                let target = match values.first() {
                    Some(Value::File(index)) => self.env.files[*index].path.clone(),
                    Some(Value::Link { target, .. }) => target.clone(),
                    Some(Value::Str(s)) => strip_brackets(s).to_string(),
                    _ => return Err("link() takes a path or a file".to_string()),
                };
                Value::Link {
                    target,
                    display: values.get(1).map(to_text),
                }
            }
            "file" => values
                .first()
                .and_then(|value| self.env.file_of(value))
                .map_or(Value::Null, Value::File),
            _ => return Err(format!("{name}() is not a function TurboVault evaluates")),
        })
    }

    fn method(&mut self, receiver: &Value, name: &str, args: &[Expr]) -> EvalResult {
        // Lambda methods evaluate their argument per item.
        if let Value::List(items) = receiver
            && matches!(name, "filter" | "map" | "reduce")
        {
            return self.lambda(items, name, args);
        }
        match name {
            "isTruthy" => return Ok(Value::Bool(truthy(receiver))),
            "toString" => return Ok(Value::Str(to_text(receiver))),
            "isType" => {
                let wanted = self.args(args)?;
                return Ok(Value::Bool(wanted.first().is_some_and(|t| {
                    to_text(t).eq_ignore_ascii_case(type_name(receiver))
                })));
            }
            _ => {}
        }
        let values = self.args(args)?;
        let text_arg = |i: usize| values.get(i).map(to_text).unwrap_or_default();
        let number_arg = |i: usize| match values.get(i) {
            Some(Value::Number(n)) => Some(*n),
            _ => None,
        };
        Ok(match (receiver, name) {
            (Value::Null, "isEmpty") => Value::Bool(true),
            (Value::Null, _) => Value::Null,

            (Value::Str(s), "contains") => Value::Bool(s.contains(&text_arg(0))),
            (Value::Str(s), "containsAll") => {
                Value::Bool(values.iter().all(|v| s.contains(&to_text(v))))
            }
            (Value::Str(s), "containsAny") => {
                Value::Bool(values.iter().any(|v| s.contains(&to_text(v))))
            }
            (Value::Str(s), "startsWith") => Value::Bool(s.starts_with(&text_arg(0))),
            (Value::Str(s), "endsWith") => Value::Bool(s.ends_with(&text_arg(0))),
            (Value::Str(s), "isEmpty") => Value::Bool(s.is_empty()),
            (Value::Str(s), "lower") => Value::Str(s.to_lowercase()),
            (Value::Str(s), "title") => Value::Str(title_case(s)),
            (Value::Str(s), "trim") => Value::Str(s.trim().to_string()),
            (Value::Str(s), "replace") => Value::Str(s.replacen(&text_arg(0), &text_arg(1), 1)),
            (Value::Str(s), "repeat") => {
                Value::Str(s.repeat(number_arg(0).unwrap_or(0.0).clamp(0.0, 10_000.0) as usize))
            }
            (Value::Str(s), "reverse") => Value::Str(s.chars().rev().collect()),
            (Value::Str(s), "slice") => {
                let chars: Vec<char> = s.chars().collect();
                let (start, end) = slice_bounds(chars.len(), number_arg(0), number_arg(1));
                Value::Str(chars[start..end].iter().collect())
            }
            (Value::Str(s), "split") => {
                let separator = text_arg(0);
                let parts: Vec<Value> = if separator.is_empty() {
                    s.chars().map(|c| Value::Str(c.to_string())).collect()
                } else {
                    s.split(separator.as_str())
                        .map(|p| Value::Str(p.to_string()))
                        .collect()
                };
                let limit = number_arg(1).map_or(parts.len(), |n| n.max(0.0) as usize);
                Value::List(parts.into_iter().take(limit).collect())
            }

            (Value::Number(_), "isEmpty") => Value::Bool(false),
            (Value::Number(n), "abs") => Value::Number(n.abs()),
            (Value::Number(n), "ceil") => Value::Number(n.ceil()),
            (Value::Number(n), "floor") => Value::Number(n.floor()),
            (Value::Number(n), "round") => {
                let digits = number_arg(0).unwrap_or(0.0).clamp(0.0, 15.0) as i32;
                let scale = 10f64.powi(digits);
                Value::Number((n * scale).round() / scale)
            }
            (Value::Number(n), "toFixed") => {
                let digits = number_arg(0).unwrap_or(0.0).clamp(0.0, 20.0) as usize;
                Value::Str(format!("{n:.digits$}"))
            }

            (Value::List(items), "contains") => Value::Bool(
                values
                    .first()
                    .is_some_and(|needle| items.iter().any(|item| self.equals(item, needle))),
            ),
            (Value::List(items), "containsAll") => Value::Bool(
                values
                    .iter()
                    .all(|needle| items.iter().any(|item| self.equals(item, needle))),
            ),
            (Value::List(items), "containsAny") => Value::Bool(
                values
                    .iter()
                    .any(|needle| items.iter().any(|item| self.equals(item, needle))),
            ),
            (Value::List(items), "isEmpty") => Value::Bool(items.is_empty()),
            (Value::List(items), "join") => Value::Str(
                items
                    .iter()
                    .map(to_text)
                    .collect::<Vec<_>>()
                    .join(&text_arg(0)),
            ),
            (Value::List(items), "reverse") => Value::List(items.iter().rev().cloned().collect()),
            (Value::List(items), "slice") => {
                let (start, end) = slice_bounds(items.len(), number_arg(0), number_arg(1));
                Value::List(items[start..end].to_vec())
            }
            (Value::List(items), "sort") => {
                let mut sorted = items.clone();
                sorted.sort_by(|a, b| self.sort_order(a, b));
                Value::List(sorted)
            }
            (Value::List(items), "unique") => {
                let mut unique: Vec<Value> = Vec::new();
                for item in items {
                    if !unique.iter().any(|seen| self.equals(seen, item)) {
                        unique.push(item.clone());
                    }
                }
                Value::List(unique)
            }
            (Value::List(items), "flat") => Value::List(flatten(items.clone())),

            (Value::Date(_), "isEmpty") => Value::Bool(false),
            (Value::Date(at), "date") => Value::Date(at.date().and_time(chrono::NaiveTime::MIN)),
            (Value::Date(at), "time") => Value::Str(at.format("%H:%M:%S").to_string()),
            (Value::Date(at), "format") => {
                Value::Str(crate::moment_format::format(*at, &text_arg(0))?)
            }

            (Value::File(index), method) => {
                let file = &self.env.files[*index];
                match method {
                    "hasTag" => Value::Bool(values.iter().any(|wanted| {
                        let wanted = to_text(wanted).trim_start_matches('#').to_lowercase();
                        file.tags.iter().any(|tag| {
                            let tag = tag.trim_start_matches('#').to_lowercase();
                            tag == wanted || tag.starts_with(&format!("{wanted}/"))
                        })
                    })),
                    "inFolder" => {
                        let folder = text_arg(0);
                        let folder = folder.trim_matches('/');
                        Value::Bool(
                            folder.is_empty() || file.path.starts_with(&format!("{folder}/")),
                        )
                    }
                    "hasProperty" => Value::Bool(file.properties.contains_key(&text_arg(0))),
                    "hasLink" => {
                        let Some(wanted) = values.first().and_then(|v| self.env.file_of(v)) else {
                            return Ok(Value::Bool(false));
                        };
                        let wanted = &self.env.files[wanted].full_path;
                        Value::Bool(file.links.iter().chain(&file.embeds).any(|(target, _)| {
                            self.env.graph.resolve(target).is_some_and(|p| p == wanted)
                        }))
                    }
                    "asLink" => Value::Link {
                        target: file.path.clone(),
                        display: values.first().map(to_text),
                    },
                    "isEmpty" => Value::Bool(false),
                    _ => return Err(format!("a file has no method {method}()")),
                }
            }

            (Value::Link { .. }, "asFile") => {
                self.env.file_of(receiver).map_or(Value::Null, Value::File)
            }
            (Value::Link { .. }, "linksTo") => Value::Bool(
                values
                    .first()
                    .is_some_and(|other| self.same_file(receiver, other)),
            ),
            (Value::Link { .. }, "isEmpty") => Value::Bool(false),

            (Value::Object(map), "isEmpty") => Value::Bool(map.is_empty()),
            (Value::Object(map), "keys") => {
                Value::List(map.keys().cloned().map(Value::Str).collect())
            }
            (Value::Object(map), "values") => Value::List(map.values().cloned().collect()),

            (other, method) => {
                return Err(format!("{} has no method {method}()", type_name(other)));
            }
        })
    }

    /// `list.filter(expr)`, `list.map(expr)` and `list.reduce(expr, acc)`:
    /// `expr` is evaluated per item with `value` and `index` bound, and
    /// `acc` for reduce.
    fn lambda(&mut self, items: &[Value], name: &str, args: &[Expr]) -> EvalResult {
        let body = arg(args, 0, name)?;
        let mut acc = match name {
            "reduce" => self.eval(arg(args, 1, "reduce")?)?,
            _ => Value::Null,
        };
        let mut out = Vec::new();
        for (index, item) in items.iter().enumerate() {
            self.vars.push(("value".to_string(), item.clone()));
            self.vars
                .push(("index".to_string(), Value::Number(index as f64)));
            self.vars.push(("acc".to_string(), acc.clone()));
            let result = self.eval(body);
            self.vars.truncate(self.vars.len() - 3);
            let result = result?;
            match name {
                "filter" if truthy(&result) => out.push(item.clone()),
                "map" => out.push(result),
                "reduce" => acc = result,
                _ => {}
            }
        }
        Ok(if name == "reduce" {
            acc
        } else {
            Value::List(out)
        })
    }

    fn binary(&mut self, op: BinaryOp, left: &Expr, right: &Expr) -> EvalResult {
        if op == BinaryOp::And {
            let l = self.eval(left)?;
            return Ok(Value::Bool(truthy(&l) && truthy(&self.eval(right)?)));
        }
        if op == BinaryOp::Or {
            let l = self.eval(left)?;
            return Ok(Value::Bool(truthy(&l) || truthy(&self.eval(right)?)));
        }
        let l = self.eval(left)?;
        let r = self.eval(right)?;
        Ok(match op {
            BinaryOp::Eq => Value::Bool(self.equals(&l, &r)),
            BinaryOp::Ne => Value::Bool(!self.equals(&l, &r)),
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
                let ordering = compare(&l, &r);
                Value::Bool(ordering.is_some_and(|o| match op {
                    BinaryOp::Lt => o == Ordering::Less,
                    BinaryOp::Le => o != Ordering::Greater,
                    BinaryOp::Gt => o == Ordering::Greater,
                    _ => o != Ordering::Less,
                }))
            }
            BinaryOp::Add | BinaryOp::Sub => arithmetic(op, &l, &r)?,
            BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem => match (&l, &r) {
                (Value::Number(a), Value::Number(b)) => Value::Number(match op {
                    BinaryOp::Mul => a * b,
                    BinaryOp::Div => a / b,
                    _ => a % b,
                }),
                (Value::Null, _) | (_, Value::Null) => Value::Null,
                _ => {
                    return Err(format!(
                        "can't use {op} on {} and {}",
                        type_name(&l),
                        type_name(&r)
                    ));
                }
            },
            BinaryOp::And | BinaryOp::Or => unreachable!("handled above"),
        })
    }

    /// Whether two values are equal, the way Bases compares them: links and
    /// files by the note they resolve to, dates with date strings.
    pub fn equals(&self, a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Link { .. } | Value::File(_), _) | (_, Value::Link { .. } | Value::File(_)) => {
                self.same_file(a, b)
            }
            (Value::Date(x), Value::Str(s)) | (Value::Str(s), Value::Date(x)) => {
                parse_date(s).is_some_and(|d| d == *x)
            }
            (Value::List(x), Value::List(y)) => {
                x.len() == y.len() && x.iter().zip(y).all(|(p, q)| self.equals(p, q))
            }
            _ => a == b,
        }
    }

    fn same_file(&self, a: &Value, b: &Value) -> bool {
        match (self.env.file_of(a), self.env.file_of(b)) {
            (Some(x), Some(y)) => x == y,
            // Neither resolves to a note: compare the names as written.
            _ => match (link_name(a, self.env), link_name(b, self.env)) {
                (Some(x), Some(y)) => x == y,
                _ => false,
            },
        }
    }

    /// Order for sorting: by value, empty values last.
    pub fn sort_order(&self, a: &Value, b: &Value) -> Ordering {
        match (a, b) {
            (Value::Null, Value::Null) => Ordering::Equal,
            (Value::Null, _) => Ordering::Greater,
            (_, Value::Null) => Ordering::Less,
            _ => compare(a, b).unwrap_or_else(|| {
                type_rank(a)
                    .cmp(&type_rank(b))
                    .then_with(|| to_text(a).to_lowercase().cmp(&to_text(b).to_lowercase()))
            }),
        }
    }
}

fn arg<'x>(args: &'x [Expr], i: usize, name: &str) -> Result<&'x Expr, String> {
    args.get(i)
        .ok_or_else(|| format!("{name}() needs at least {} argument(s)", i + 1))
}

fn arithmetic(op: BinaryOp, l: &Value, r: &Value) -> EvalResult {
    let duration = |v: &Value| match v {
        Value::Duration(d) => Some(*d),
        Value::Str(s) => parse_duration(s),
        _ => None,
    };
    let sign = if op == BinaryOp::Sub { -1 } else { 1 };
    Ok(match (l, r) {
        (Value::Number(a), Value::Number(b)) => Value::Number(a + f64::from(sign) * b),
        (Value::Date(at), other) if duration(other).is_some() => {
            let d = duration(other).unwrap_or_default();
            Value::Date(shift(
                *at,
                d.months * i64::from(sign),
                d.millis * i64::from(sign),
            )?)
        }
        (other, Value::Date(at)) if op == BinaryOp::Add && duration(other).is_some() => {
            let d = duration(other).unwrap_or_default();
            Value::Date(shift(*at, d.months, d.millis)?)
        }
        (Value::Date(a), Value::Date(b)) if op == BinaryOp::Sub => {
            Value::Number((*a - *b).num_milliseconds() as f64)
        }
        (Value::Duration(a), Value::Duration(b)) => Value::Duration(Duration {
            months: a.months + i64::from(sign) * b.months,
            millis: a.millis + i64::from(sign) * b.millis,
        }),
        (Value::Null, _) | (_, Value::Null) => Value::Null,
        (Value::Str(_), _) | (_, Value::Str(_)) if op == BinaryOp::Add => {
            Value::Str(format!("{}{}", to_text(l), to_text(r)))
        }
        _ => {
            return Err(format!(
                "can't use {op} on {} and {}",
                type_name(l),
                type_name(r)
            ));
        }
    })
}

fn shift(at: NaiveDateTime, months: i64, millis: i64) -> Result<NaiveDateTime, String> {
    let shifted = if months >= 0 {
        at.checked_add_months(Months::new(months as u32))
    } else {
        at.checked_sub_months(Months::new(months.unsigned_abs() as u32))
    };
    shifted
        .and_then(|d| d.checked_add_signed(ChronoDuration::milliseconds(millis)))
        .ok_or_else(|| "the date moves out of range".to_string())
}

/// `1 day`, `2w`, `1M`, `-3 hours`, `1 year 2 months`: whole numbers of
/// years (`y`), months (`M`), weeks (`w`), days (`d`), hours (`h`), minutes
/// (`m`) and seconds (`s`), with or without spaces and long names.
pub fn parse_duration(text: &str) -> Option<Duration> {
    let mut duration = Duration::default();
    let mut rest = text.trim();
    if rest.is_empty() {
        return None;
    }
    while !rest.is_empty() {
        let negative = rest.starts_with('-');
        let digits_start = usize::from(negative || rest.starts_with('+'));
        let digits_end = rest[digits_start..]
            .find(|c: char| !c.is_ascii_digit())
            .map_or(rest.len(), |i| i + digits_start);
        let amount: i64 = rest[digits_start..digits_end].parse().ok()?;
        let amount = if negative { -amount } else { amount };
        rest = rest[digits_end..].trim_start();
        let unit_end = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        let unit = &rest[..unit_end];
        rest = rest[unit_end..].trim_start_matches([' ', ',']);
        match unit {
            "M" => duration.months += amount,
            "m" => duration.millis += amount * 60_000,
            _ => match unit.to_lowercase().as_str() {
                "y" | "year" | "years" => duration.months += amount * 12,
                "month" | "months" => duration.months += amount,
                "w" | "week" | "weeks" => duration.millis += amount * 7 * 86_400_000,
                "d" | "day" | "days" => duration.millis += amount * 86_400_000,
                "h" | "hour" | "hours" => duration.millis += amount * 3_600_000,
                "minute" | "minutes" => duration.millis += amount * 60_000,
                "s" | "second" | "seconds" => duration.millis += amount * 1000,
                _ => return None,
            },
        }
    }
    Some(duration)
}

/// A date or date-time string, as properties and `date()` hold them.
pub fn parse_date(text: &str) -> Option<NaiveDateTime> {
    let text = text.trim();
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return Some(date.and_time(chrono::NaiveTime::MIN));
    }
    for format in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%dT%H:%M:%S%.f",
    ] {
        if let Ok(at) = NaiveDateTime::parse_from_str(text, format) {
            return Some(at);
        }
    }
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|at| at.with_timezone(&chrono::Local).naive_local())
}

fn compare(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.partial_cmp(y),
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
        (Value::Date(x), Value::Date(y)) => Some(x.cmp(y)),
        (Value::Date(x), Value::Str(s)) => parse_date(s).map(|y| x.cmp(&y)),
        (Value::Str(s), Value::Date(y)) => parse_date(s).map(|x| x.cmp(y)),
        _ => None,
    }
}

fn type_rank(value: &Value) -> u8 {
    match value {
        Value::Bool(_) => 0,
        Value::Number(_) => 1,
        Value::Date(_) => 2,
        Value::Duration(_) => 3,
        Value::Str(_) => 4,
        Value::Link { .. } | Value::File(_) => 5,
        Value::List(_) => 6,
        Value::Object(_) => 7,
        Value::Null => 8,
    }
}

/// The type name `isType()` compares against.
pub fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::Str(_) => "string",
        Value::Date(_) => "date",
        Value::Duration(_) => "duration",
        Value::List(_) => "list",
        Value::Object(_) => "object",
        Value::Link { .. } => "link",
        Value::File(_) => "file",
    }
}

/// Whether a value counts as true: not null, false, 0, empty text or an
/// empty list.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => *n != 0.0 && !n.is_nan(),
        Value::Str(s) => !s.is_empty(),
        Value::List(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
        _ => true,
    }
}

/// A value as text, the way it would print.
pub fn to_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => number_text(*n),
        Value::Str(s) => s.clone(),
        Value::Date(at) => date_text(*at),
        Value::Duration(d) => format!("{} months {} ms", d.months, d.millis),
        Value::List(items) => items.iter().map(to_text).collect::<Vec<_>>().join(", "),
        Value::Object(map) => serde_json::to_string(
            &map.iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(to_text(v))))
                .collect::<serde_json::Map<_, _>>(),
        )
        .unwrap_or_default(),
        Value::Link { target, display } => match display {
            Some(display) => format!("[[{target}|{display}]]"),
            None => format!("[[{target}]]"),
        },
        // A file prints as its path; the env isn't at hand here, so the
        // caller turns files into text through `to_json`.
        Value::File(_) => String::new(),
    }
}

fn number_text(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        n.to_string()
    }
}

fn date_text(at: NaiveDateTime) -> String {
    if at.time() == chrono::NaiveTime::MIN {
        at.format("%Y-%m-%d").to_string()
    } else {
        at.format("%Y-%m-%dT%H:%M:%S").to_string()
    }
}

/// A value as JSON for a result row.
pub fn to_json(value: &Value, env: &Env<'_>) -> serde_json::Value {
    use serde_json::Value as J;
    match value {
        Value::Null => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Number(n) => {
            if n.fract() == 0.0 && n.abs() < 9e15 {
                J::from(*n as i64)
            } else {
                serde_json::Number::from_f64(*n).map_or(J::Null, J::Number)
            }
        }
        Value::List(items) => J::Array(items.iter().map(|v| to_json(v, env)).collect()),
        Value::Object(map) => J::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), to_json(v, env)))
                .collect(),
        ),
        Value::File(index) => J::String(env.files[*index].path.clone()),
        other => J::String(to_text(other)),
    }
}

/// A property value as JSON holds it, with `[[links]]` read as links.
pub fn from_json(value: &serde_json::Value) -> Value {
    match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => n.as_f64().map_or(Value::Null, Value::Number),
        serde_json::Value::String(s) => match wikilink(s) {
            Some((target, display)) => Value::Link { target, display },
            None => Value::Str(s.clone()),
        },
        serde_json::Value::Array(items) => Value::List(items.iter().map(from_json).collect()),
        serde_json::Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), from_json(v))).collect())
        }
    }
}

fn property(properties: &serde_json::Map<String, serde_json::Value>, name: &str) -> Value {
    properties.get(name).map_or(Value::Null, from_json)
}

fn links(links: &[(String, Option<String>)]) -> Value {
    Value::List(
        links
            .iter()
            .map(|(target, display)| Value::Link {
                target: target.clone(),
                display: display.clone(),
            })
            .collect(),
    )
}

/// `[[Target]]` or `[[Target|Shown]]`, as a whole string.
fn wikilink(text: &str) -> Option<(String, Option<String>)> {
    let inner = text.trim().strip_prefix("[[")?.strip_suffix("]]")?;
    if inner.contains("[[") || inner.contains("]]") {
        return None;
    }
    Some(match inner.split_once('|') {
        Some((target, display)) => (target.to_string(), Some(display.to_string())),
        None => (inner.to_string(), None),
    })
}

fn strip_brackets(text: &str) -> &str {
    let text = text.trim();
    let inner = text
        .strip_prefix("[[")
        .and_then(|t| t.strip_suffix("]]"))
        .unwrap_or(text);
    inner.split('|').next().unwrap_or(inner)
}

/// A link or file's name for comparing when it doesn't resolve: lowercase,
/// without `.md` or a heading.
fn link_name(value: &Value, env: &Env<'_>) -> Option<String> {
    let raw = match value {
        Value::Link { target, .. } => target.clone(),
        Value::Str(s) => strip_brackets(s).to_string(),
        Value::File(index) => env.files[*index].path.clone(),
        _ => return None,
    };
    let raw = raw.split('#').next().unwrap_or(&raw).to_lowercase();
    Some(raw.strip_suffix(".md").unwrap_or(&raw).to_string())
}

fn flatten(values: Vec<Value>) -> Vec<Value> {
    let mut out = Vec::new();
    for value in values {
        match value {
            Value::List(items) => out.extend(flatten(items)),
            other => out.push(other),
        }
    }
    out
}

/// JavaScript `slice` bounds over `len` items: negative counts from the
/// end, and the end defaults to the length.
fn slice_bounds(len: usize, start: Option<f64>, end: Option<f64>) -> (usize, usize) {
    let clamp = |n: f64| -> usize {
        let n = n as i64;
        let len = len as i64;
        (if n < 0 { len + n } else { n }).clamp(0, len) as usize
    };
    let start = start.map_or(0, clamp);
    let end = end.map_or(len, clamp);
    (start, end.max(start))
}

fn title_case(text: &str) -> String {
    text.split(' ')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(
            parse_duration("1 day"),
            Some(Duration {
                months: 0,
                millis: 86_400_000
            })
        );
        assert_eq!(parse_duration("1M").map(|d| d.months), Some(1));
        assert_eq!(parse_duration("2m").map(|d| d.millis), Some(120_000));
        assert_eq!(
            parse_duration("1 year 2 months").map(|d| d.months),
            Some(14)
        );
        assert_eq!(parse_duration("-1w").map(|d| d.millis), Some(-604_800_000));
        assert_eq!(parse_duration("soon"), None);
    }

    #[test]
    fn slices_like_javascript() {
        assert_eq!(slice_bounds(5, Some(1.0), None), (1, 5));
        assert_eq!(slice_bounds(5, Some(-2.0), None), (3, 5));
        assert_eq!(slice_bounds(5, Some(3.0), Some(1.0)), (3, 3));
    }
}
