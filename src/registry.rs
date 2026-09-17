use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::SystemTime;

use rand::Rng;

use crate::error::{FwError, Result};
use crate::ipc::protocol::{RouteView, StopSelector};
use crate::metrics::RouteMetrics;
use crate::slug::{generate_unique_slug, normalize_slug};

#[derive(Debug, Clone)]
pub struct Route {
    pub slug: String,
    pub target: SocketAddr,
    pub owner_session: u64,
    #[allow(dead_code)]
    pub owner_pid: u32,
    #[allow(dead_code)]
    pub created_at: SystemTime,
    pub metrics: Arc<RouteMetrics>,
}

impl Route {
    pub fn port(&self) -> u16 {
        self.target.port()
    }

    pub fn view(&self, base_domain: &str) -> RouteView {
        RouteView::new(self.slug.clone(), self.port(), base_domain)
    }
}

#[derive(Debug, Default)]
pub struct RouteRegistry {
    by_slug: HashMap<String, Route>,
    slug_by_port: HashMap<u16, String>,
    slugs_by_session: HashMap<u64, HashSet<String>>,
}

impl RouteRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.by_slug.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_slug.is_empty()
    }

    /// Atomically validate collisions and insert a route while the caller holds the registry lock.
    pub fn register(
        &mut self,
        port: u16,
        requested_slug: Option<&str>,
        owner_session: u64,
        owner_pid: u32,
    ) -> Result<Route> {
        let mut rng = rand::rng();
        self.register_with_rng(port, requested_slug, owner_session, owner_pid, &mut rng)
    }

    /// Injectable-RNG form of [`Self::register`] for deterministic tests.
    pub fn register_with_rng<R: Rng + ?Sized>(
        &mut self,
        port: u16,
        requested_slug: Option<&str>,
        owner_session: u64,
        owner_pid: u32,
        rng: &mut R,
    ) -> Result<Route> {
        if port == 0 {
            return Err(FwError::InvalidPort(port.to_string()));
        }
        if let Some(active_slug) = self.slug_by_port.get(&port) {
            return Err(FwError::DuplicatePort {
                port,
                slug: active_slug.clone(),
            });
        }

        let slug = match requested_slug {
            Some(slug) => {
                let slug = normalize_slug(slug)?;
                if self.by_slug.contains_key(&slug) {
                    return Err(FwError::DuplicateSlug(slug));
                }
                slug
            }
            None => generate_unique_slug(rng, |candidate| self.by_slug.contains_key(candidate))?,
        };

        let route = Route {
            slug: slug.clone(),
            target: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            owner_session,
            owner_pid,
            created_at: SystemTime::now(),
            metrics: Arc::new(RouteMetrics::new()),
        };

        self.by_slug.insert(slug.clone(), route.clone());
        self.slug_by_port.insert(port, slug.clone());
        self.slugs_by_session
            .entry(owner_session)
            .or_default()
            .insert(slug);

        Ok(route)
    }

    pub fn route_for_slug(&self, slug: &str) -> Option<&Route> {
        self.by_slug.get(slug)
    }

    pub fn route_for_port(&self, port: u16) -> Option<&Route> {
        self.slug_by_port
            .get(&port)
            .and_then(|slug| self.by_slug.get(slug))
    }

    /// Copy only the proxy target so no registry borrow needs to cross network I/O.
    #[cfg(test)]
    pub fn target_for_slug(&self, slug: &str) -> Option<SocketAddr> {
        self.route_for_slug(slug).map(|route| route.target)
    }

    pub fn resolve(&self, selector: &StopSelector) -> Option<&Route> {
        match selector {
            StopSelector::Slug(slug) => self.route_for_slug(slug),
            StopSelector::Port(port) => self.route_for_port(*port),
        }
    }

    pub fn list(&self, base_domain: &str) -> Vec<RouteView> {
        let mut routes: Vec<_> = self
            .by_slug
            .values()
            .map(|route| route.view(base_domain))
            .collect();
        routes.sort_unstable_by(|left, right| left.slug.cmp(&right.slug));
        routes
    }

    /// Remove a route regardless of owner. Repeated cleanup is a no-op.
    #[cfg(test)]
    pub fn remove_slug(&mut self, slug: &str) -> Option<Route> {
        let route = self.by_slug.remove(slug)?;
        self.slug_by_port.remove(&route.port());

        let remove_session_index =
            if let Some(slugs) = self.slugs_by_session.get_mut(&route.owner_session) {
                slugs.remove(slug);
                slugs.is_empty()
            } else {
                false
            };
        if remove_session_index {
            self.slugs_by_session.remove(&route.owner_session);
        }

        Some(route)
    }

    /// Remove a route only if the specified session owns it.
    #[cfg(test)]
    pub fn remove_owned_slug(&mut self, owner_session: u64, slug: &str) -> Option<Route> {
        if self.route_for_slug(slug)?.owner_session != owner_session {
            return None;
        }
        self.remove_slug(slug)
    }

    /// Idempotently remove every route owned by a disconnected IPC session.
    pub fn remove_session(&mut self, owner_session: u64) -> Vec<Route> {
        let Some(slugs) = self.slugs_by_session.remove(&owner_session) else {
            return Vec::new();
        };

        let mut removed = Vec::with_capacity(slugs.len());
        for slug in slugs {
            if let Some(route) = self.by_slug.remove(&slug) {
                self.slug_by_port.remove(&route.port());
                removed.push(route);
            }
        }
        removed.sort_unstable_by(|left, right| left.slug.cmp(&right.slug));
        removed
    }

    pub fn clear(&mut self) -> Vec<Route> {
        let mut routes: Vec<_> = self.by_slug.drain().map(|(_, route)| route).collect();
        self.slug_by_port.clear();
        self.slugs_by_session.clear();
        routes.sort_unstable_by(|left, right| left.slug.cmp(&right.slug));
        routes
    }
}

#[cfg(test)]
mod tests {
    use rand::{SeedableRng, rngs::StdRng};

    use super::*;
    use crate::slug::generate_slug;

    fn register(registry: &mut RouteRegistry, port: u16, slug: &str, session: u64) -> Route {
        registry
            .register(port, Some(slug), session, session as u32)
            .unwrap()
    }

    #[test]
    fn inserts_and_resolves_all_indexes() {
        let mut registry = RouteRegistry::new();
        let route = register(&mut registry, 4321, "Green-Apple", 10);

        assert_eq!(route.slug, "green-apple");
        assert_eq!(route.target, SocketAddr::from((Ipv4Addr::LOCALHOST, 4321)));
        assert_eq!(registry.target_for_slug("green-apple"), Some(route.target));
        assert_eq!(registry.route_for_port(4321).unwrap().slug, "green-apple");
        assert_eq!(
            registry
                .resolve(&StopSelector::Port(4321))
                .unwrap()
                .owner_session,
            10
        );
    }

    #[test]
    fn rejects_duplicate_slug_and_port() {
        let mut registry = RouteRegistry::new();
        register(&mut registry, 4321, "apple-pen", 1);

        assert!(matches!(
            registry.register(8080, Some("APPLE-PEN"), 2, 2),
            Err(FwError::DuplicateSlug(slug)) if slug == "apple-pen"
        ));
        assert!(matches!(
            registry.register(4321, Some("other-route"), 2, 2),
            Err(FwError::DuplicatePort { port: 4321, slug }) if slug == "apple-pen"
        ));
    }

    #[test]
    fn generated_registration_retries_collision() {
        let mut preview_rng = StdRng::seed_from_u64(27);
        let occupied = generate_slug(&mut preview_rng);
        let mut registry = RouteRegistry::new();
        register(&mut registry, 1001, &occupied, 1);
        let mut rng = StdRng::seed_from_u64(27);

        let generated = registry
            .register_with_rng(1002, None, 2, 2, &mut rng)
            .unwrap();

        assert_ne!(generated.slug, occupied);
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn disconnect_cleanup_removes_every_owned_route_and_indexes() {
        let mut registry = RouteRegistry::new();
        register(&mut registry, 3001, "first-route", 7);
        register(&mut registry, 3002, "second-route", 7);
        register(&mut registry, 3003, "other-route", 8);

        let removed = registry.remove_session(7);

        assert_eq!(removed.len(), 2);
        assert!(registry.route_for_port(3001).is_none());
        assert!(registry.route_for_slug("second-route").is_none());
        assert_eq!(registry.route_for_port(3003).unwrap().owner_session, 8);
    }

    #[test]
    fn cleanup_operations_are_idempotent() {
        let mut registry = RouteRegistry::new();
        register(&mut registry, 4321, "apple-pen", 1);

        assert_eq!(registry.remove_session(1).len(), 1);
        assert!(registry.remove_session(1).is_empty());
        assert!(registry.remove_slug("apple-pen").is_none());
        assert!(registry.is_empty());
    }

    #[test]
    fn refuses_cleanup_by_non_owner() {
        let mut registry = RouteRegistry::new();
        register(&mut registry, 4321, "apple-pen", 1);
        assert!(registry.remove_owned_slug(2, "apple-pen").is_none());
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn list_is_stable_and_contains_public_urls() {
        let mut registry = RouteRegistry::new();
        register(&mut registry, 8080, "zebra-route", 1);
        register(&mut registry, 4321, "apple-route", 2);

        let routes = registry.list("mytunnel.me");
        assert_eq!(routes[0].slug, "apple-route");
        assert_eq!(routes[0].local_url, "http://127.0.0.1:4321");
        assert_eq!(routes[0].public_url, "https://apple-route.mytunnel.me");
    }
}
