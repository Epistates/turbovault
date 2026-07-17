//! Git worktree fanout lifecycle tools.

use std::ops::Deref;

use super::super::*;

#[derive(Clone)]
pub(super) struct FanoutProvider(CoreToolHandler);

impl FanoutProvider {
    pub(super) fn new(core: CoreToolHandler) -> Self {
        Self(core)
    }

    fn scratch_path(id: &str) -> PathBuf {
        std::env::temp_dir().join(format!("turbovault-fanout-{}-{id}", std::process::id()))
    }

    fn parse_strategy(value: Option<&str>) -> McpResult<Option<GitMergeStrategy>> {
        match value {
            None => Ok(None),
            Some("merge-commit") | Some("merge_commit") => Ok(Some(GitMergeStrategy::MergeCommit)),
            Some("fast-forward") | Some("fast_forward") => Ok(Some(GitMergeStrategy::FastForward)),
            Some(other) => Err(McpError::invalid_request(format!(
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

#[turbomcp::server(name = "obsidian-vault", version = "1.5.0")]
impl FanoutProvider {
    #[tool(
        description = "Open an isolated Git worktree for parallel agent writes. Fanout provides isolation, while batch_execute provides all-or-nothing multi-file atomicity.",
        usage = "Requires write_backend=git. Switch agents to the returned fanout_vault, then call commit_fanout or abandon_fanout.",
        related = ["commit_fanout", "abandon_fanout", "batch_execute", "set_active_vault"],
        tags = ["write", "git"],
        destructive = true,
    )]
    async fn begin_fanout(&self, merge_strategy: Option<String>) -> McpResult<serde_json::Value> {
        let base_vault = self.get_active_vault_name().await?;
        let base_config = self
            .multi_vault_mgr
            .get_active_vault_config()
            .await
            .map_err(|error| McpError::internal(error.to_string()))?;
        if base_config.write_backend != WriteBackend::Git {
            return Err(McpError::invalid_request(
                "fanouts require write_backend=git".to_string(),
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
                return Err(McpError::invalid_request(
                    "the active vault already participates in a fanout".to_string(),
                ));
            }
        }

        let fanout_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let scratch_path = Self::scratch_path(&fanout_id);
        let locks = self.get_or_init_git_locks(&base_vault).await;
        let repo_path = base_config.path.clone();
        let scratch_for_task = scratch_path.clone();
        let id_for_task = fanout_id.clone();
        let info = tokio::task::spawn_blocking(move || {
            let repo = VaultRepo::open_with_locks(&repo_path, locks)
                .map_err(|error| McpError::internal(error.to_string()))?;
            repo.open_fanout_worktree(&id_for_task, &scratch_for_task)
                .map_err(|error| McpError::internal(error.to_string()))
        })
        .await
        .map_err(|error| McpError::internal(error.to_string()))??;

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
            serde_json::json!({
                "fanout_id": fanout_id,
                "base_vault": base_vault,
                "fanout_vault": fanout_vault,
                "worktree_path": scratch_path,
            }),
        )
        .with_next_steps(&["set_active_vault", "commit_fanout", "abandon_fanout"])
        .to_json()
    }

    #[tool(
        description = "Merge the active fanout back into its base vault and clean up the scratch worktree.",
        usage = "Call with either the base or fanout vault active. merge_strategy overrides the vault's Git setting.",
        related = ["begin_fanout", "abandon_fanout", "batch_execute"],
        tags = ["write", "git"],
        destructive = true,
    )]
    async fn commit_fanout(
        &self,
        merge_strategy: Option<String>,
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let (base_vault, record) = self.active_record().await.ok_or_else(|| {
            McpError::invalid_request("no active fanout; call begin_fanout first".to_string())
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
        let result = tokio::task::spawn_blocking(move || {
            let repo = VaultRepo::open_with_locks(&path, locks)
                .map_err(|error| McpError::internal(error.to_string()))?;
            repo.merge_fanout_back(&info, strategy, commit_message.as_deref())
                .map_err(|error| McpError::internal(error.to_string()))
        })
        .await
        .map_err(|error| McpError::internal(error.to_string()))??;

        let _ = self
            .multi_vault_mgr
            .remove_vault(&record.fanout_vault_name)
            .await;
        self.active_fanouts.write().await.remove(&base_vault);
        StandardResponse::new(
            base_vault,
            "commit_fanout",
            serde_json::json!({
                "fanout_id": record.fanout_id,
                "merge_commit": result.merge_commit.map(|oid| oid.to_string()),
                "tip_before": result.tip_before.to_string(),
                "tip_after": result.tip_after.to_string(),
            }),
        )
        .with_next_step("quick_health_check")
        .to_json()
    }

    #[tool(
        description = "Discard the active fanout and remove its worktree without changing the base vault.",
        usage = "Use when parallel work should not be merged. Safe when no fanout is active.",
        related = ["begin_fanout", "commit_fanout"],
        tags = ["write", "git"],
        destructive = true,
    )]
    async fn abandon_fanout(&self) -> McpResult<serde_json::Value> {
        let Some((base_vault, record)) = self.active_record().await else {
            return StandardResponse::new(
                self.get_active_vault_name().await.unwrap_or_default(),
                "abandon_fanout",
                serde_json::json!({"was_active": false}),
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
        tokio::task::spawn_blocking(move || {
            let repo = VaultRepo::open_with_locks(&path, locks)
                .map_err(|error| McpError::internal(error.to_string()))?;
            repo.abandon_fanout_by_info(&info)
                .map_err(|error| McpError::internal(error.to_string()))
        })
        .await
        .map_err(|error| McpError::internal(error.to_string()))??;
        let _ = self
            .multi_vault_mgr
            .remove_vault(&record.fanout_vault_name)
            .await;
        self.active_fanouts.write().await.remove(&base_vault);
        StandardResponse::new(
            base_vault,
            "abandon_fanout",
            serde_json::json!({"was_active": true, "fanout_id": record.fanout_id}),
        )
        .to_json()
    }

    #[tool(
        description = "List orphan fanout worktrees left by interrupted server sessions.",
        usage = "Diagnostic only; this tool never mutates Git state.",
        related = ["begin_fanout", "abandon_fanout"],
        tags = ["read", "git"],
        read_only = true,
    )]
    async fn list_orphan_fanouts(&self, vault: Option<String>) -> McpResult<serde_json::Value> {
        let orphans = self
            .scan_orphan_fanouts(vault.as_deref())
            .await
            .map_err(|error| McpError::internal(error.to_string()))?;
        StandardResponse::new(
            self.get_active_vault_name().await.unwrap_or_default(),
            "list_orphan_fanouts",
            serde_json::json!({"count": orphans.len(), "orphans": orphans}),
        )
        .to_json()
    }
}
