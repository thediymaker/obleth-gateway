//! MCP servers.

use super::*;

fn mcp_server_from_row(row: &PgRow) -> Result<McpServer> {
    Ok(McpServer {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        upstream_url: row.try_get("upstream_url")?,
        auth_header: cipher().decrypt_opt(row.try_get("auth_header")?)?,
        enabled: row.try_get("enabled")?,
        allowed_tenants: row.try_get("allowed_tenants")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

impl Store {
    pub async fn create_mcp_server(
        &self,
        name: &str,
        upstream_url: &str,
        auth_header: Option<&str>,
        allowed_tenants: &[Uuid],
    ) -> Result<McpServer> {
        let auth_header = cipher().encrypt_opt(auth_header);
        let row = sqlx::query(
            "insert into mcp_servers (id, name, upstream_url, auth_header, allowed_tenants)
             values ($1, $2, $3, $4, $5)
             returning id, name, upstream_url, auth_header, enabled, allowed_tenants, created_at, updated_at",
        )
        .bind(Uuid::new_v4())
        .bind(name)
        .bind(upstream_url)
        .bind(auth_header)
        .bind(allowed_tenants)
        .fetch_one(&self.pool)
        .await?;
        mcp_server_from_row(&row)
    }

    pub async fn list_mcp_servers(&self) -> Result<Vec<McpServer>> {
        let rows = sqlx::query(
            "select id, name, upstream_url, auth_header, enabled, allowed_tenants, created_at, updated_at
             from mcp_servers order by name",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(mcp_server_from_row).collect()
    }

    pub async fn get_mcp_server(&self, id: Uuid) -> Result<McpServer> {
        let row = sqlx::query(
            "select id, name, upstream_url, auth_header, enabled, allowed_tenants, created_at, updated_at
             from mcp_servers where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        mcp_server_from_row(&row)
    }

    pub async fn update_mcp_server(
        &self,
        id: Uuid,
        upstream_url: &str,
        auth_header: Option<&str>,
        enabled: bool,
        allowed_tenants: &[Uuid],
    ) -> Result<McpServer> {
        let auth_header = cipher().encrypt_opt(auth_header);
        let row = sqlx::query(
            "update mcp_servers set upstream_url = $2, auth_header = $3, enabled = $4, allowed_tenants = $5, updated_at = now()
             where id = $1
             returning id, name, upstream_url, auth_header, enabled, allowed_tenants, created_at, updated_at",
        )
        .bind(id)
        .bind(upstream_url)
        .bind(auth_header)
        .bind(enabled)
        .bind(allowed_tenants)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        mcp_server_from_row(&row)
    }

    /// Rename an MCP server and carry the new name into every model's
    /// `tool_servers` grant, in one transaction. Returns the server and the
    /// models whose grants changed, so the caller can republish them.
    pub async fn rename_mcp_server(
        &self,
        id: Uuid,
        new_name: &str,
    ) -> Result<(McpServer, Vec<ModelRoute>)> {
        let mut tx = self.pool.begin().await?;
        let old: String = sqlx::query("select name from mcp_servers where id = $1 for update")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StoreError::NotFound)?
            .try_get("name")?;
        let row = sqlx::query(
            "update mcp_servers set name = $2, updated_at = now() where id = $1
             returning id, name, upstream_url, auth_header, enabled, allowed_tenants, created_at, updated_at",
        )
        .bind(id)
        .bind(new_name)
        .fetch_one(&mut *tx)
        .await?;
        let server = mcp_server_from_row(&row)?;
        let rows = sqlx::query(&format!(
            "update models
                set tool_servers = (tool_servers - $1) || jsonb_build_array($2::text), updated_at = now()
              where tool_servers ? $1
             returning {MODEL_COLUMNS}"),
        )
        .bind(&old)
        .bind(new_name)
        .fetch_all(&mut *tx)
        .await?;
        let models = rows
            .iter()
            .map(model_from_row)
            .collect::<Result<Vec<_>>>()?;
        tx.commit().await?;
        Ok((server, models))
    }

    /// Delete an MCP server, returning its name so the cache can be invalidated.
    pub async fn delete_mcp_server(&self, id: Uuid) -> Result<String> {
        let row = sqlx::query("delete from mcp_servers where id = $1 returning name")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound)?;
        Ok(row.try_get("name")?)
    }

    /// Remove an MCP server's name from every model's `tool_servers` grant,
    /// returning the updated models so the caller can re-sync their resolved
    /// views. Used when a server is deleted: a grant naming an unregistered
    /// server fails tool discovery on every request, and the dashboard can no
    /// longer show (or clear) it once the server's checkbox is gone.
    pub async fn strip_tool_server_grants(&self, server_name: &str) -> Result<Vec<ModelRoute>> {
        let rows = sqlx::query(&format!(
            "update models
                set tool_servers = tool_servers - $1, updated_at = now()
              where tool_servers ? $1
             returning {MODEL_COLUMNS}"
        ))
        .bind(server_name)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(model_from_row).collect()
    }

    /// All enabled MCP servers as (name, resolved), for warming the cache.
    pub async fn all_resolved_mcp_servers(&self) -> Result<Vec<(String, ResolvedMcpServer)>> {
        let rows = sqlx::query(
            "select name, upstream_url, auth_header, enabled, allowed_tenants
             from mcp_servers where enabled = true",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                let name: String = row.try_get("name")?;
                Ok((
                    name.clone(),
                    ResolvedMcpServer {
                        name,
                        upstream_url: row.try_get("upstream_url")?,
                        auth_header: cipher().decrypt_opt(row.try_get("auth_header")?)?,
                        enabled: row.try_get("enabled")?,
                        allowed_tenants: row.try_get("allowed_tenants")?,
                    },
                ))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{default_test_fields, serial, FixtureGuard};

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// Deleting an MCP server must strip its grant from every model without
    /// touching grants for other servers (the stale-grant bug: a deleted
    /// server's name lingered in `tool_servers` forever, failing tool
    /// discovery on every request with no way to clear it from the UI).
    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// Renaming a server carries the new name into every model that granted it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn rename_mcp_server_moves_model_grants() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let old = format!("mcp-{}", Uuid::new_v4());
        let new = format!("mcp-{}", Uuid::new_v4());
        let other = format!("mcp-{}", Uuid::new_v4());
        let server = store
            .create_mcp_server(&old, "http://127.0.0.1:1/mcp", None, &[])
            .await
            .expect("create mcp server");
        fixtures.track_mcp_server(server.id);
        let name = format!("m-{}", Uuid::new_v4());
        let model = store
            .create_model(&NewModel::new(
                &name,
                ModelFields {
                    tool_servers: vec![old.clone(), other.clone()],
                    ..default_test_fields()
                },
            ))
            .await
            .expect("create model");
        fixtures.track_model(model.id);

        let (renamed, models) = store
            .rename_mcp_server(server.id, &new)
            .await
            .expect("rename");
        assert_eq!(renamed.name, new);
        let changed = models
            .iter()
            .find(|m| m.id == model.id)
            .expect("model regranted");
        assert!(changed.tool_servers.contains(&new));
        assert!(changed.tool_servers.contains(&other));
        assert!(!changed.tool_servers.contains(&old));
        assert!(matches!(
            store.rename_mcp_server(Uuid::new_v4(), "x").await,
            Err(StoreError::NotFound)
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn strip_tool_server_grants_removes_only_the_deleted_server() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let doomed = format!("mcp-{}", Uuid::new_v4());
        let kept = format!("mcp-{}", Uuid::new_v4());
        let server = store
            .create_mcp_server(&doomed, "http://127.0.0.1:1/mcp", None, &[])
            .await
            .expect("create mcp server");
        fixtures.track_mcp_server(server.id);

        // One model granted both servers, one granted only the surviving one.
        let both_name = format!("m-{}", Uuid::new_v4());
        let both = store
            .create_model(&NewModel::new(
                &both_name,
                ModelFields {
                    tool_servers: vec![doomed.clone(), kept.clone()],
                    ..default_test_fields()
                },
            ))
            .await
            .expect("create model with both grants");
        fixtures.track_model(both.id);

        let kept_only_name = format!("m-{}", Uuid::new_v4());
        let kept_only = store
            .create_model(&NewModel::new(
                &kept_only_name,
                ModelFields {
                    tool_servers: vec![kept.clone()],
                    ..default_test_fields()
                },
            ))
            .await
            .expect("create model with kept grant");
        fixtures.track_model(kept_only.id);

        store
            .delete_mcp_server(server.id)
            .await
            .expect("delete mcp server");
        let stripped = store
            .strip_tool_server_grants(&doomed)
            .await
            .expect("strip grants");

        // Only the model that held the doomed grant is touched, and it keeps
        // its other grant.
        assert_eq!(stripped.len(), 1, "exactly one model updated");
        assert_eq!(stripped[0].id, both.id);
        assert_eq!(stripped[0].tool_servers, vec![kept.clone()]);

        let untouched = store.get_model(kept_only.id).await.expect("get model");
        assert_eq!(untouched.tool_servers, vec![kept.clone()]);

        // Idempotent: a second strip finds nothing to update.
        let again = store
            .strip_tool_server_grants(&doomed)
            .await
            .expect("strip again");
        assert!(again.is_empty(), "second strip is a no-op");
    }
}
