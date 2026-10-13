//! Git worktree fanout lifecycle tools.

use std::ops::Deref;

use super::super::*;

#[derive(Clone)]
pub(super) struct FanoutProvider(CoreToolHandler);

impl FanoutProvider {
    pub(super) fn new(core: CoreToolHandler) -> Self {
        Self(core)
    }

    /// A path for a fanout's scratch worktree: `worktree` inside a new
    /// directory only this user can enter, under the project's cache
    /// directory (the system temp directory without one). The worktree is a
    /// full copy of the vault, so its directory is created fresh, with an
    /// unguessable name, and fails rather than reuse anything already there.
    async fn scratch_path(&self) -> McpResult<PathBuf> {
        let parent = match self.persistent_cache.read().await.as_ref() {
            Some(cache) => cache.project_cache_dir().join("fanout"),
            None => std::env::temp_dir(),
        };
        tokio::task::spawn_blocking(move || create_private_scratch_dir(&parent))
            .await
            .map_err(|error| McpError::internal(error.to_string()))?
            .map(|dir| dir.join("worktree"))
            .map_err(|error| {
                McpError::internal(format!("cannot create a fanout scratch directory: {error}"))
            })
    }

    /// Remove the directory [`Self::scratch_path`] made for `worktree`,
    /// once the worktree inside it is gone.
    fn remove_scratch_dir(worktree: &Path) {
        if let Some(dir) = worktree.parent().filter(|dir| {
            dir.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("turbovault-fanout-"))
        }) {
            let _ = std::fs::remove_dir_all(worktree);
            if let Err(error) = std::fs::remove_dir(dir) {
                log::warn!(
                    "fanout scratch directory {} was not removed: {error}",
                    dir.display()
                );
            }
        }
    }

    fn parse_strategy(value: Option<&str>) -> McpResult<Option<GitMergeStrategy>> {
        match value {
            None => Ok(None),
            Some("merge-commit") | Some("merge_commit") => Ok(Some(GitMergeStrategy::MergeCommit)),
            Some("fast-forward") | Some("fast_forward") => Ok(Some(GitMergeStrategy::FastForward)),
            Some(other) => Err(McpError::invalid_params(format!(
                "invalid merge_strategy '{other}': use 'merge-commit' or 'fast-forward'"
            ))),
        }
    }

    fn configured_strategy(value: ConfigMergeStrategy) -> GitMergeStrategy {
        match value {
            ConfigMergeStrategy::MergeCommit => GitMergeStrategy::MergeCommit,
            ConfigMergeStrategy::FastForward => GitMergeStrategy::FastForward,
        }
    }

    async fn active_record(&self) -> Option<(String, ActiveFanoutRecord)> {
        let active = self.get_active_vault_name().await.ok()?;
        let fanouts = self.active_fanouts.read().await;
        if let Some(record) = fanouts.get(&active) {
            return Some((active, record.clone()));
        }
        fanouts.iter().find_map(|(base, record)| {
            (record.fanout_vault_name == active).then(|| (base.clone(), record.clone()))
        })
    }
}

impl Deref for FanoutProvider {
    type Target = CoreToolHandler;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Response payload for `begin_fanout`.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct FanoutBegun {
    fanout_id: String,
    base_vault: String,
    /// The vault to make active to work in the fanout.
    fanout_vault: String,
    worktree_path: PathBuf,
}

/// Response payload for `commit_fanout`.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct FanoutCommitted {
    fanout_id: String,
    /// `null` when the fanout's work fast-forwarded.
    merge_commit: Option<String>,
    tip_before: String,
    tip_after: String,
}

/// Response payload for `abandon_fanout`.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct FanoutAbandoned {
    /// Whether there was a fanout to abandon.
    was_active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fanout_id: Option<String>,
}

/// Response payload for `list_orphan_fanouts`.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct OrphanFanouts {
    count: usize,
    orphans: Vec<OrphanFanoutEntry>,
}

#[turbomcp::server(name = "obsidian-vault", version = env!("CARGO_PKG_VERSION"))]
impl FanoutProvider {
    #[tool(
        description = "Open a private Git worktree of the active vault, registered as its own vault, so a long piece of work can be written there without touching the vault until commit_fanout merges it back (or abandon_fanout drops it). Switch to it with set_active_vault. Git-backed vaults only; for a few files at once, batch_execute is simpler.",
        title = "Begin Fanout",
        tags = ["write", "git"],
        read_only = false,
        destructive = false,
        idempotent = false,
        open_world = false,
        output_schema = StandardResponse<FanoutBegun>,
    )]
    async fn begin_fanout(
        &self,
        #[description(
            "Checked now, but the merge uses the merge_strategy given to commit_fanout: `merge-commit` or `fast-forward`."
        )]
        merge_strategy: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let base_vault = self.get_active_vault_name().await?;
        let base_config = self
            .multi_vault_mgr
            .get_active_vault_config()
            .await
            .map_err(|error| McpError::internal(error.to_string()))?;
        if base_config.write_backend != WriteBackend::Git {
            return Err(McpError::unavailable(
                "fanouts require write_backend=git; this vault writes files directly".to_string(),
            ));
        }
        Self::parse_strategy(merge_strategy.as_deref())?;
        {
            let fanouts = self.active_fanouts.read().await;
            if fanouts.contains_key(&base_vault)
                || fanouts
                    .values()
                    .any(|record| record.fanout_vault_name == base_vault)
            {
                return Err(McpError::invalid_params(
                    "the active vault already participates in a fanout".to_string(),
                ));
            }
        }

        let fanout_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let scratch_path = self.scratch_path().await?;
        let locks = self.get_or_init_git_locks(&base_vault).await;
        let repo_path = base_config.path.clone();
        let scratch_for_task = scratch_path.clone();
        let id_for_task = fanout_id.clone();
        let opened = tokio::task::spawn_blocking(move || {
            let repo = VaultRepo::open_with_locks(&repo_path, locks)
                .map_err(|error| McpError::internal(error.to_string()))?;
            repo.open_fanout_worktree(&id_for_task, &scratch_for_task)
                .map_err(|error| McpError::internal(error.to_string()))
        })
        .await
        .map_err(|error| McpError::internal(error.to_string()))?;
        let info = match opened {
            Ok(info) => info,
            Err(error) => {
                Self::remove_scratch_dir(&scratch_path);
                return Err(error);
            }
        };

        let fanout_vault = format!("{base_vault}-fanout-{fanout_id}");
        let config = VaultConfig::builder(&fanout_vault, &scratch_path)
            .write_backend(WriteBackend::Git)
            .build()
            .map_err(to_mcp_error)?;
        self.multi_vault_mgr
            .add_vault(config)
            .await
            .map_err(|error| McpError::internal(error.to_string()))?;
        self.active_fanouts.write().await.insert(
            base_vault.clone(),
            ActiveFanoutRecord {
                fanout_id: fanout_id.clone(),
                info,
                fanout_vault_name: fanout_vault.clone(),
            },
        );

        StandardResponse::new(
            &base_vault,
            "begin_fanout",
            FanoutBegun {
                fanout_id,
                base_vault: base_vault.clone(),
                fanout_vault,
                worktree_path: scratch_path,
            },
        )
        .with_next_steps(&["set_active_vault", "commit_fanout", "abandon_fanout"])
        .to_json()
    }

    #[tool(
        description = "Merge the active fanout's work back into its base vault, then remove the worktree and unregister it. The worktree is removed even if the merge fails. Works from the base vault or the fanout itself.",
        title = "Commit Fanout",
        tags = ["write", "git"],
        read_only = false,
        destructive = true,
        idempotent = false,
        open_world = false,
        output_schema = StandardResponse<FanoutCommitted>,
    )]
    async fn commit_fanout(
        &self,
        #[description(
            "`merge-commit` records a merge commit; `fast-forward` moves the base branch to the fanout's tip and fails if the base moved meanwhile. Default the vault's git.merge_strategy, else `merge-commit`."
        )]
        merge_strategy: Option<String>,
        #[description(
            "Message for the merge commit. A message naming the fanout is used when omitted."
        )]
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let (base_vault, record) = self.active_record().await.ok_or_else(|| {
            McpError::invalid_params("no active fanout; call begin_fanout first".to_string())
        })?;
        let base_config = self
            .multi_vault_mgr
            .get_vault_config(&base_vault)
            .await
            .map_err(|error| McpError::internal(error.to_string()))?;
        let strategy = Self::parse_strategy(merge_strategy.as_deref())?.unwrap_or_else(|| {
            base_config
                .git
                .as_ref()
                .map(|git| Self::configured_strategy(git.merge_strategy))
                .unwrap_or(GitMergeStrategy::MergeCommit)
        });
        let locks = self.get_or_init_git_locks(&base_vault).await;
        let path = base_config.path.clone();
        let info = record.info.clone();
        let worktree = info.worktree_path.clone();
        let result = tokio::task::spawn_blocking(move || {
            let repo = VaultRepo::open_with_locks(&path, locks)
                .map_err(|error| McpError::internal(error.to_string()))?;
            repo.merge_fanout_back(&info, strategy, commit_message.as_deref())
                .map_err(|error| McpError::internal(error.to_string()))
        })
        .await
        .map_err(|error| McpError::internal(error.to_string()))??;
        // merge_fanout_back removes the worktree whether or not the merge
        // landed; its private directory goes with it.
        Self::remove_scratch_dir(&worktree);

        let _ = self
            .multi_vault_mgr
            .remove_vault(&record.fanout_vault_name)
            .await;
        self.active_fanouts.write().await.remove(&base_vault);
        StandardResponse::new(
            base_vault,
            "commit_fanout",
            FanoutCommitted {
                fanout_id: record.fanout_id,
                merge_commit: result.merge_commit.map(|oid| oid.to_string()),
                tip_before: result.tip_before.to_string(),
                tip_after: result.tip_after.to_string(),
            },
        )
        .with_next_step("quick_health_check")
        .to_json()
    }

    #[tool(
        description = "Throw away the active fanout: remove its worktree and everything written there, leaving the base vault unchanged. Does nothing if no fanout is active.",
        title = "Abandon Fanout",
        tags = ["write", "git"],
        read_only = false,
        destructive = true,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<FanoutAbandoned>,
    )]
    async fn abandon_fanout(&self) -> McpResult<serde_json::Value> {
        let Some((base_vault, record)) = self.active_record().await else {
            return StandardResponse::new(
                self.get_active_vault_name().await.unwrap_or_default(),
                "abandon_fanout",
                FanoutAbandoned {
                    was_active: false,
                    fanout_id: None,
                },
            )
            .to_json();
        };
        let config = self
            .multi_vault_mgr
            .get_vault_config(&base_vault)
            .await
            .map_err(|error| McpError::internal(error.to_string()))?;
        let locks = self.get_or_init_git_locks(&base_vault).await;
        let path = config.path.clone();
        let info = record.info.clone();
        let worktree = info.worktree_path.clone();
        tokio::task::spawn_blocking(move || {
            let repo = VaultRepo::open_with_locks(&path, locks)
                .map_err(|error| McpError::internal(error.to_string()))?;
            repo.abandon_fanout_by_info(&info)
                .map_err(|error| McpError::internal(error.to_string()))
        })
        .await
        .map_err(|error| McpError::internal(error.to_string()))??;
        Self::remove_scratch_dir(&worktree);
        let _ = self
            .multi_vault_mgr
            .remove_vault(&record.fanout_vault_name)
            .await;
        self.active_fanouts.write().await.remove(&base_vault);
        StandardResponse::new(
            base_vault,
            "abandon_fanout",
            FanoutAbandoned {
                was_active: true,
                fanout_id: Some(record.fanout_id),
            },
        )
        .to_json()
    }

    #[tool(
        description = "List fanout worktrees left behind by a server session that ended before committing or abandoning them.",
        title = "List Orphan Fanouts",
        tags = ["read", "git"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<OrphanFanouts>,
    )]
    async fn list_orphan_fanouts(
        &self,
        #[description("Only this vault's orphans, by name. Omit to check every Git-backed vault.")]
        vault: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let orphans = self
            .scan_orphan_fanouts(vault.as_deref())
            .await
            .map_err(|error| McpError::internal(error.to_string()))?;
        StandardResponse::new(
            self.get_active_vault_name().await.unwrap_or_default(),
            "list_orphan_fanouts",
            OrphanFanouts {
                count: orphans.len(),
                orphans,
            },
        )
        .to_json()
    }
}

/// Create a new directory under `parent` that only this user can enter, with
/// a random name, failing if anything already has that name.
fn create_private_scratch_dir(parent: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(parent)?;
    let dir = parent.join(format!(
        "turbovault-fanout-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&dir)?;
    Ok(dir)
}
