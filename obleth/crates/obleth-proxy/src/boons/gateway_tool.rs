//! What the gateway-executed tools (`generate_image`, `web_search`) share: who
//! owns a tool name, arming a tool into the request and the tool loop, and the
//! event each call leaves for the trace.

use std::collections::HashMap;

use obleth_config::{Boon, ResolvedModel};
use serde_json::Value;

/// One gateway-executed tool, ready to arm into a request.
pub(super) struct GatewayTool {
    pub boon: Boon,
    /// The function name the model calls.
    pub name: &'static str,
    /// The tool loop's synthetic-server entry that routes the call back to
    /// the gateway.
    pub server: &'static str,
    /// The tool definition merged into the request's `tools`.
    pub def: Value,
    /// The prompt section for a plain chat client that brought no tools.
    pub nudge: &'static str,
    /// Log a lost name at warn (an operator should see it) rather than debug
    /// (clients that bring their own tool of this name are common).
    pub warn_on_collision: bool,
}

impl GatewayTool {
    /// Arm the tool unless something else owns its name: merge its
    /// definition into the request (with the nudge when `nudge`), and add its
    /// synthetic-server entry to `servers` so the tool loop executes its
    /// calls. Returns whether it was armed. The injection and the map entry
    /// are decided from one [`collides`] answer, so the request body and the
    /// loop's dispatch map never disagree about who owns the name.
    pub(super) fn arm(
        self,
        route: &ResolvedModel,
        nudge: bool,
        json: &mut Value,
        servers: &mut Option<HashMap<String, String>>,
    ) -> bool {
        if collides(json, servers.as_ref(), self.name) {
            if self.warn_on_collision {
                tracing::warn!(
                    tool = self.name,
                    model = %route.model_name,
                    boon = self.boon.name(),
                    "tool name is owned by a client-supplied tool or a granted MCP tool; \
                     existing tool wins, boon inactive for this request"
                );
            } else {
                tracing::debug!(
                    tool = self.name,
                    model = %route.model_name,
                    boon = self.boon.name(),
                    "tool name is owned by a client-supplied tool or a granted MCP tool; \
                     existing tool wins, boon inactive for this request"
                );
            }
            return false;
        }
        inject_tool_def(
            json,
            self.def,
            nudge.then_some(self.nudge),
            route.supports_system_messages,
        );
        servers
            .get_or_insert_with(HashMap::new)
            .insert(self.name.to_string(), self.server.to_string());
        true
    }
}

/// Whether `name` is already owned: a tool the client itself defined (agentic
/// clients often bring their own), or one the MCP tool loop mapped from a
/// granted server. The existing owner always wins.
pub(super) fn collides(
    json: &Value,
    servers: Option<&HashMap<String, String>>,
    name: &str,
) -> bool {
    let client_has_it = json
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| {
            tools
                .iter()
                .any(|t| t.pointer("/function/name").and_then(Value::as_str) == Some(name))
        });
    client_has_it || servers.is_some_and(|m| m.contains_key(name))
}

/// Merge `def` into the request's tools, keeping the client's own, and add
/// `nudge` as a prompt section when given.
fn inject_tool_def(json: &mut Value, def: Value, nudge: Option<&str>, supports_system: bool) {
    if let Some(obj) = json.as_object_mut() {
        match obj.get_mut("tools").and_then(Value::as_array_mut) {
            Some(existing) => existing.push(def),
            None => {
                obj.insert("tools".into(), Value::Array(vec![def]));
            }
        }
    }
    if let Some(nudge) = nudge {
        super::structured::inject_prompt_section(json, nudge, supports_system);
    }
}

/// The model was granted a gateway-tool boon that is configured, but cannot
/// call functions, so it would silently get no tool.
pub(super) fn lacks_function_calling(route: &ResolvedModel, boon: Boon, active: bool) -> bool {
    route.has_boon(boon) && active && !route.supports_function_calling
}

/// One call of a gateway tool, for its span under `boon:tool_loop`. Kept by
/// the tool's context and drained by the loop's trace (which owns no
/// recorder while the calls run).
pub(super) struct GatewayToolEvent {
    /// `boon:image_generation`, `boon:web_search`.
    pub span: &'static str,
    pub upstream_ms: u32,
    pub ok: bool,
    pub attributes: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &'static str) -> GatewayTool {
        GatewayTool {
            boon: Boon::WebSearch,
            name,
            server: "__test__",
            def: json!({ "type": "function", "function": { "name": name } }),
            nudge: "NUDGE",
            warn_on_collision: false,
        }
    }

    fn route() -> ResolvedModel {
        let mut route = super::super::tests::endpoint_only_route("http://unused/v1");
        route.supports_system_messages = true;
        route
    }

    /// Both gateway tools decide ownership the same way: a client tool of the
    /// name, or a granted MCP server's, wins; another name is no collision.
    #[test]
    fn collision_is_decided_the_same_for_each_tool() {
        for name in ["generate_image", "web_search"] {
            let plain = json!({ "messages": [] });
            assert!(!collides(&plain, None, name));
            let client = json!({ "tools": [{ "type": "function", "function": { "name": name } }] });
            assert!(collides(&client, None, name));
            let mcp = HashMap::from([(name.to_string(), "an-mcp-server".to_string())]);
            assert!(collides(&plain, Some(&mcp), name));
            let other = HashMap::from([("something_else".to_string(), "s".to_string())]);
            assert!(!collides(&plain, Some(&other), name));
        }
    }

    #[test]
    fn arming_merges_the_tool_maps_it_and_nudges_only_when_asked() {
        let mut body = json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "tools": [{ "type": "function", "function": { "name": "client_tool" } }]
        });
        let mut servers = None;
        assert!(tool("web_search").arm(&route(), false, &mut body, &mut servers));
        let names: Vec<&str> = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();
        assert_eq!(names, vec!["client_tool", "web_search"]);
        assert_eq!(body["messages"].as_array().unwrap().len(), 1, "no nudge");
        assert_eq!(servers.unwrap()["web_search"], "__test__");

        let mut plain = json!({ "messages": [{ "role": "user", "content": "hi" }] });
        let mut servers = None;
        assert!(tool("web_search").arm(&route(), true, &mut plain, &mut servers));
        assert_eq!(plain["messages"][0]["content"], "NUDGE");
    }

    /// A lost name leaves the body and the map exactly as they were.
    #[test]
    fn a_tool_whose_name_is_taken_is_not_armed() {
        let mut body = json!({
            "messages": [],
            "tools": [{ "type": "function", "function": { "name": "web_search", "parameters": {} } }]
        });
        let before = body.clone();
        let mut servers = None;
        assert!(!tool("web_search").arm(&route(), true, &mut body, &mut servers));
        assert_eq!(body, before);
        assert!(servers.is_none());
    }
}
