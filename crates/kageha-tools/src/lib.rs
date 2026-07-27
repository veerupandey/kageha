//! Framework-independent composition. Names must resolve to exactly one configured adapter.
pub mod browser;
pub mod memory;
use kageha_contracts::execution::*;
use serde_json::Value;
use std::{io, sync::Arc};
pub struct Composite {
    ports: Vec<Arc<dyn ToolPort + Send + Sync>>,
}
impl Composite {
    pub fn new(ports: Vec<Arc<dyn ToolPort + Send + Sync>>) -> io::Result<Self> {
        if ports.is_empty() || ports.len() > 32 {
            return Err(io::Error::other(
                "Configure one to thirty-two tool adapters",
            ));
        }
        let port = Self { ports };
        port.discover()?;
        Ok(port)
    }
    fn catalog(&self, context: Option<&ToolContext>) -> io::Result<(Vec<Value>, Vec<usize>)> {
        let mut specs = Vec::new();
        let mut routes = Vec::new();
        let mut names = std::collections::HashSet::new();
        for (index, port) in self.ports.iter().enumerate() {
            let tools = if let Some(context) = context {
                port.discover_in(context)?
            } else {
                port.discover()?
            };
            for tool in tools {
                let name = tool["function"]["name"]
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
                    .ok_or_else(|| io::Error::other("Invalid tool descriptor"))?;
                if !names.insert(name.to_owned()) {
                    return Err(io::Error::other(
                        "Tool name is exposed by more than one adapter",
                    ));
                }
                if specs.len() >= 512 {
                    return Err(io::Error::other("Composite tool catalog exceeds limit"));
                }
                specs.push(tool);
                routes.push(index);
            }
        }
        Ok((specs, routes))
    }
    fn route(
        &self,
        context: Option<&ToolContext>,
        name: &str,
    ) -> io::Result<&(dyn ToolPort + Send + Sync)> {
        let (specs, routes) = self.catalog(context)?;
        let index = specs
            .iter()
            .position(|v| v["function"]["name"] == name)
            .ok_or_else(|| io::Error::other("Tool is not exposed by a configured adapter"))?;
        Ok(self.ports[routes[index]].as_ref())
    }
}
impl ToolPort for Composite {
    fn execute_authorized_in(
        &self,
        context: &ToolContext,
        call: &ToolCall,
        authority: &ToolBinding,
        control: &Control<'_>,
    ) -> io::Result<ToolReply> {
        self.route(Some(context), &call.name)?
            .execute_authorized_in(context, call, authority, control)
    }

    fn dispatch_access(&self, call: &ToolCall) -> io::Result<DispatchAccess> {
        self.route(None, &call.name)?.dispatch_access(call)
    }
    fn dispatch_access_in(
        &self,
        context: &ToolContext,
        call: &ToolCall,
    ) -> io::Result<DispatchAccess> {
        self.route(Some(context), &call.name)?
            .dispatch_access_in(context, call)
    }

    fn discover(&self) -> io::Result<Vec<Value>> {
        self.catalog(None).map(|v| v.0)
    }
    fn discover_in(&self, context: &ToolContext) -> io::Result<Vec<Value>> {
        self.catalog(Some(context)).map(|v| v.0)
    }
    fn execute(&self, call: &ToolCall, control: &Control<'_>) -> io::Result<ToolReply> {
        self.route(None, &call.name)?.execute(call, control)
    }
    fn execute_in(
        &self,
        context: &ToolContext,
        call: &ToolCall,
        control: &Control<'_>,
    ) -> io::Result<ToolReply> {
        self.route(Some(context), &call.name)?
            .execute_in(context, call, control)
    }
    fn binding(&self, context: &ToolContext, call: &ToolCall) -> io::Result<ToolBinding> {
        self.route(Some(context), &call.name)?
            .binding(context, call)
    }
    fn mutation_binding(
        &self,
        context: &ToolContext,
        call: &ToolCall,
    ) -> io::Result<Option<ToolBinding>> {
        self.route(Some(context), &call.name)?
            .mutation_binding(context, call)
    }
    fn release_context(&self, session: &str) -> io::Result<()> {
        for port in &self.ports {
            port.release_context(session)?;
        }
        Ok(())
    }
    fn erase_context(&self, session: &str) -> io::Result<()> {
        for port in &self.ports {
            port.erase_context(session)?;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::AtomicBool;
    struct Port(&'static str);
    impl ToolPort for Port {
        fn dispatch_access(&self, _: &ToolCall) -> io::Result<DispatchAccess> {
            Ok(DispatchAccess::Resource(self.0.into()))
        }
        fn discover(&self) -> io::Result<Vec<Value>> {
            Ok(vec![
                json!({"type":"function","function":{"name":self.0,"parameters":{"type":"object"}}}),
            ])
        }
        fn execute(&self, _: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
            panic!("context must be forwarded")
        }
        fn execute_in(
            &self,
            scope: &ToolContext,
            _: &ToolCall,
            _: &Control<'_>,
        ) -> io::Result<ToolReply> {
            Ok(ToolReply::success(format!(
                "{}:{}:{}",
                self.0, scope.session, scope.turn
            )))
        }
    }
    #[test]
    fn routes_scoped_calls_and_rejects_ambiguous_catalogs() {
        assert!(Composite::new(vec![Arc::new(Port("a")), Arc::new(Port("a"))]).is_err());
        let tools = Composite::new(vec![Arc::new(Port("a")), Arc::new(Port("b"))]).unwrap();
        let scope = ToolContext {
            session: "chat".into(),
            turn: "turn".into(),
        };
        let mut call = ToolCall {
            id: "1".into(),
            name: "b".into(),
            arguments: json!({}),
        };
        assert_eq!(
            tools
                .execute_in(
                    &scope,
                    &call,
                    &Control {
                        cancelled: &AtomicBool::new(false),
                        deadline: None
                    }
                )
                .unwrap()
                .content,
            "b:chat:turn"
        );
        assert_eq!(
            tools.dispatch_access_in(&scope, &call).unwrap(),
            DispatchAccess::Resource("b".into())
        );
        call.name = "missing".into();
        assert!(tools.binding(&scope, &call).is_err());
    }
}

#[cfg(test)]
mod mutation_projection_tests {
    use super::*;
    use serde_json::json;
    struct ReviewedRead;
    impl ToolPort for ReviewedRead {
        fn discover(&self) -> io::Result<Vec<Value>> {
            Ok(vec![json!({"function":{"name":"read"}})])
        }
        fn binding(&self, _: &ToolContext, _: &ToolCall) -> io::Result<ToolBinding> {
            Ok(ToolBinding {
                binding: "reviewed".into(),
                generation: 1,
                resource: "file".into(),
                class: kageha_contracts::policy::ActionClass::Restricted,
                detail: "Mandatory review of a read".into(),
                fingerprint: "fixed".into(),
            })
        }
        fn mutation_binding(
            &self,
            _: &ToolContext,
            _: &ToolCall,
        ) -> io::Result<Option<ToolBinding>> {
            Ok(None)
        }
        fn execute(&self, _: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
            Ok(ToolReply::success("read".into()))
        }
    }
    #[test]
    fn composition_preserves_read_semantics_when_review_is_mandatory() {
        let c = Composite::new(vec![Arc::new(ReviewedRead)]).unwrap();
        let context = ToolContext {
            session: "s".into(),
            turn: "t".into(),
        };
        let call = ToolCall {
            id: "c".into(),
            name: "read".into(),
            arguments: json!({}),
        };
        assert_eq!(
            c.binding(&context, &call).unwrap().class,
            kageha_contracts::policy::ActionClass::Restricted
        );
        assert!(c.mutation_binding(&context, &call).unwrap().is_none());
    }
}
