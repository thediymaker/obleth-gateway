-- Idempotent: safe to re-run.
-- Which tenants may call an MCP server through /mcp/<name>.
--
-- Until now any valid key reached every enabled server, with the upstream
-- credential the operator saved for it. A server is now reached only by keys
-- of the tenants listed here, and the list starts empty: a server registered
-- before this column existed is closed to direct calls until an operator
-- names the tenants that may use it. Tools a model is granted from a server
-- (models.tool_servers) are not affected: the gateway makes those calls
-- itself, for whoever may use the model.
alter table mcp_servers
    add column if not exists allowed_tenants uuid[] not null default '{}';
