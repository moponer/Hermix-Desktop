//! # hermix-context
//!
//! Service registry with **reversible registration**.
//!
//! ## Why reversible
//!
//! A plugin that registers things — a tool, a model provider, a hook
//! — also has to unregister them when it is unloaded, and every one
//! of those unregistrations is a chance to forget one. Miss one and
//! the kernel keeps calling into a plugin that no longer exists.
//!
//! So registration is an *effect that unwinds*: handing out a
//! [`ServiceGuard`] instead of a bare `()` means the cleanup is tied
//! to a value's lifetime, and the compiler runs it. Dropping the
//! guard removes the service. There is no separate teardown list to
//! keep in sync, and a plugin cannot half-unload.
//!
//! ## The overwrite trap
//!
//! The obvious implementation — "on drop, remove this key" — is
//! wrong. If plugin A registers a service and plugin B later replaces
//! it, dropping A's guard would remove B's registration, silently
//! unprovisioning a plugin that is still running.
//!
//! Guards therefore remember the exact value they installed and only
//! remove it if it is still the one registered (compared by pointer).
//! A superseded guard's drop is a no-op. See
//! [`tests::superseded_guard_drop_does_not_remove_the_replacement`].
//!
//! ## Boot
//!
//! [`boot`] assembles the kernel seams (Fs / Shell / Sandbox / Transport /
//! ExecWorld) into a root scope. Registration is reversible there too, so
//! dropping the assembled process unregisters every provider with it.

pub mod boot;

pub use boot::{Boot, BootProfile, BootSeams};

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

type ServiceMap = HashMap<TypeId, Arc<dyn Any + Send + Sync>>;

struct Inner {
    services: ServiceMap,
    /// Parent scope, consulted when a lookup misses locally. Lets a
    /// plugin see kernel services without being able to mutate them.
    parent: Option<Context>,
    /// Callbacks run when this scope is disposed, in reverse
    /// registration order.
    disposers: Vec<Box<dyn FnOnce() + Send>>,
}

/// A service scope.
///
/// Cloning shares the same scope: clones see each other's
/// registrations and keep them alive.
#[derive(Clone)]
pub struct Context {
    inner: Arc<Mutex<Inner>>,
}

impl Context {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                services: HashMap::new(),
                parent: None,
                disposers: Vec::new(),
            })),
        }
    }

    /// Create a child scope that inherits this one's services.
    ///
    /// A plugin gets a child: it can add services and shadow inherited
    /// ones locally, but dropping the child cannot remove anything the
    /// parent registered.
    pub fn child(&self) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                services: HashMap::new(),
                parent: Some(self.clone()),
                disposers: Vec::new(),
            })),
        }
    }

    /// Register a service and get the guard that owns it.
    ///
    /// The service is visible immediately. Replacing an existing
    /// registration is allowed; the previous guard becomes inert.
    pub fn provide<T: Any + Send + Sync>(&self, value: T) -> ServiceGuard {
        let key = TypeId::of::<T>();
        let installed: Arc<dyn Any + Send + Sync> = Arc::new(value);
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.services.insert(key, installed.clone());
        drop(inner);
        ServiceGuard {
            ctx: self.clone(),
            key,
            installed,
        }
    }

    /// Look up a service, checking parent scopes on a local miss.
    pub fn get<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        let key = TypeId::of::<T>();
        let mut scope = Some(self.clone());
        while let Some(current) = scope {
            let found = {
                let inner = current.inner.lock().unwrap_or_else(|e| e.into_inner());
                inner.services.get(&key).cloned()
            };
            if let Some(value) = found {
                return value.downcast::<T>().ok();
            }
            scope = {
                let inner = current.inner.lock().unwrap_or_else(|e| e.into_inner());
                inner.parent.clone()
            };
        }
        None
    }

    /// `true` when the service is registered in this scope or an
    /// ancestor.
    pub fn has<T: Any + Send + Sync>(&self) -> bool {
        self.get::<T>().is_some()
    }

    /// Register a cleanup callback, run when this scope is disposed.
    ///
    /// For anything whose lifetime is not expressible as a service
    /// value — a spawned task, an open socket, a temp directory.
    pub fn on_dispose(&self, f: impl FnOnce() + Send + 'static) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.disposers.push(Box::new(f));
    }

    /// Tear the scope down: run disposers, then release every service
    /// registered here.
    ///
    /// Disposers run in reverse order so that dependants are cleaned
    /// up before their dependencies.
    pub fn dispose(&self) {
        let disposers = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.services.clear();
            std::mem::take(&mut inner.disposers)
        };
        for f in disposers.into_iter().rev() {
            f();
        }
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

/// Owns a registration. Dropping it unregisters the service.
///
/// Inert if the registration was superseded by a later
/// `provide` of the same type — see the module docs.
pub struct ServiceGuard {
    ctx: Context,
    key: TypeId,
    /// The exact value this guard installed, so a superseded guard
    /// can tell that the current value is not its own.
    installed: Arc<dyn Any + Send + Sync>,
}

impl ServiceGuard {
    fn unregister(&self) {
        let mut inner = self.ctx.inner.lock().unwrap_or_else(|e| e.into_inner());
        let is_mine = inner
            .services
            .get(&self.key)
            .is_some_and(|current| Arc::ptr_eq(current, &self.installed));
        if is_mine {
            inner.services.remove(&self.key);
        } else {
            tracing::debug!(
                "context: guard dropped but its registration was already superseded; \
                 leaving the current value in place"
            );
        }
    }
}

impl Drop for ServiceGuard {
    fn drop(&mut self) {
        self.unregister();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Config {
        name: String,
    }

    #[derive(Debug)]
    struct Counter;

    #[test]
    fn provided_service_is_visible() {
        let ctx = Context::new();
        let _g = ctx.provide(Config {
            name: "kernel".into(),
        });
        assert_eq!(ctx.get::<Config>().unwrap().name, "kernel");
        assert!(ctx.has::<Config>());
    }

    #[test]
    fn dropping_the_guard_unregisters() {
        let ctx = Context::new();
        {
            let _g = ctx.provide(Config { name: "tmp".into() });
            assert!(ctx.has::<Config>());
        }
        assert!(
            !ctx.has::<Config>(),
            "the effect must unwind when its guard is dropped"
        );
    }

    #[test]
    fn superseded_guard_drop_does_not_remove_the_replacement() {
        let ctx = Context::new();
        let first = ctx.provide(Config { name: "first".into() });
        let _second = ctx.provide(Config { name: "second".into() });

        // The first plugin unloads while the second is still live.
        drop(first);

        assert_eq!(
            ctx.get::<Config>().unwrap().name,
            "second",
            "an unloaded plugin must not unprovision the one that replaced it"
        );
    }

    #[test]
    fn child_sees_parent_services_and_can_shadow_them() {
        let parent = Context::new();
        let _pg = parent.provide(Config { name: "parent".into() });

        let child = parent.child();
        assert_eq!(child.get::<Config>().unwrap().name, "parent");

        let _cg = child.provide(Config { name: "child".into() });
        assert_eq!(child.get::<Config>().unwrap().name, "child");
        assert_eq!(
            parent.get::<Config>().unwrap().name,
            "parent",
            "a child must not be able to overwrite its parent"
        );
    }

    #[test]
    fn child_registrations_do_not_leak_into_the_parent() {
        let parent = Context::new();
        {
            let child = parent.child();
            let _g = child.provide(Counter);
            assert!(child.has::<Counter>());
        }
        assert!(!parent.has::<Counter>());
    }

    #[test]
    fn dispose_runs_callbacks_in_reverse_order() {
        let ctx = Context::new();
        let order = Arc::new(Mutex::new(Vec::new()));

        let o = order.clone();
        ctx.on_dispose(move || o.lock().unwrap().push("first"));
        let o = order.clone();
        ctx.on_dispose(move || o.lock().unwrap().push("second"));

        ctx.dispose();
        assert_eq!(
            *order.lock().unwrap(),
            vec!["second".to_string(), "first".to_string()],
            "dependants must be cleaned up before their dependencies"
        );
    }

    #[test]
    fn dispose_releases_the_scope_services() {
        let ctx = Context::new();
        let _g = ctx.provide(Config { name: "gone".into() });
        ctx.dispose();
        assert!(!ctx.has::<Config>());
    }
}
