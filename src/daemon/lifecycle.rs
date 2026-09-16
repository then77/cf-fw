use std::sync::Arc;

use tokio::sync::RwLock;

use crate::proxy::{ProxyRoute, RouteLookup};
use crate::registry::RouteRegistry;

#[derive(Clone)]
pub struct RegistryLookup {
    pub registry: Arc<RwLock<RouteRegistry>>,
}

impl RouteLookup for RegistryLookup {
    fn lookup<'a>(
        &'a self,
        slug: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ProxyRoute>> + Send + 'a>> {
        Box::pin(async move {
            let registry = self.registry.read().await;
            registry.route_for_slug(slug).map(|route| ProxyRoute {
                target: route.target,
                metrics: route.metrics.clone(),
            })
        })
    }
}
