//! Route allocation seam for vfkit sandboxes.

use local_sandbox_core::{
    CreateSandboxRouteRequest, SandboxId, SandboxResult, SandboxRoute, SandboxRouteProtocol,
};

/// Fakeable route allocation boundary.
pub(crate) trait RouteAllocator: Send + Sync {
    fn allocate(
        &self,
        sandbox_id: SandboxId,
        requests: &[CreateSandboxRouteRequest],
    ) -> SandboxResult<Vec<SandboxRoute>>;

    fn release(&self, sandbox_id: SandboxId, routes: &[SandboxRoute]) -> SandboxResult<()>;
}

/// Deterministic route allocator for local loopback routes.
pub(crate) struct LocalRouteAllocator {
    host: String,
}

impl LocalRouteAllocator {
    #[must_use]
    pub(crate) fn new(host: String) -> Self {
        Self { host }
    }
}

impl RouteAllocator for LocalRouteAllocator {
    fn allocate(
        &self,
        _sandbox_id: SandboxId,
        requests: &[CreateSandboxRouteRequest],
    ) -> SandboxResult<Vec<SandboxRoute>> {
        Ok(requests
            .iter()
            .map(|request| SandboxRoute {
                name: request.name.clone(),
                protocol: request.protocol,
                target_port: request.target_port,
                url: format!(
                    "{}://{}:{}",
                    protocol_scheme(request.protocol),
                    self.host,
                    request.target_port
                ),
            })
            .collect())
    }

    fn release(&self, _sandbox_id: SandboxId, _routes: &[SandboxRoute]) -> SandboxResult<()> {
        Ok(())
    }
}

pub(crate) const fn protocol_scheme(protocol: SandboxRouteProtocol) -> &'static str {
    match protocol {
        SandboxRouteProtocol::Http => "http",
        SandboxRouteProtocol::Https => "https",
        SandboxRouteProtocol::Tcp => "tcp",
    }
}
