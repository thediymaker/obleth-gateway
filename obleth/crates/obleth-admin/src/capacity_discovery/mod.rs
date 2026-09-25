//! The `discovered` capacity mode: a model's pool size derived from its live
//! backend instead of typed in.
//!
//! This module holds the gateway's discovery settings, which the Management
//! API checks model writes against.

use std::sync::Arc;

use obleth_config::capacity::DiscoveryPolicy;
use obleth_config::CapacityDiscoveryConfig;

/// Handle to the gateway's capacity discovery. Cheap to clone.
#[derive(Clone)]
pub struct CapacityDiscovery {
    inner: Arc<Inner>,
}

struct Inner {
    settings: CapacityDiscoveryConfig,
}

impl CapacityDiscovery {
    pub fn new(settings: CapacityDiscoveryConfig) -> Self {
        CapacityDiscovery {
            inner: Arc::new(Inner { settings }),
        }
    }

    /// Discovery off, no namespaces, no default selector.
    pub fn disabled() -> Self {
        Self::new(CapacityDiscoveryConfig::default())
    }

    pub fn settings(&self) -> &CapacityDiscoveryConfig {
        &self.inner.settings
    }

    /// What a model write is checked against.
    pub fn policy(&self) -> DiscoveryPolicy<'_> {
        DiscoveryPolicy {
            namespaces: &self.inner.settings.namespaces,
            default_selector: &self.inner.settings.default_selector,
        }
    }
}
