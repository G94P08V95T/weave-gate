// SPDX-License-Identifier: MIT OR Apache-2.0
// This file is part of WeaveGate.
// WeaveGate — frontend gateway and static file server.
// Copyright (C) 2019-present Jose Quintana <joseluisq.net>

use crate::settings::file::Proxy;
use crate::Result;

/// v1 routes document returned by the control plane.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct RoutesDocument {
    /// Schema version; must be `1`.
    pub version: u32,
    /// Per-application route groups.
    pub apps: Vec<AppRoutes>,
}

/// Routes for a single application (`appId`).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct AppRoutes {
    /// Application identifier (static files under `root/{id}/`).
    pub id: String,
    /// Proxy rules for this application.
    pub routes: Vec<RouteEntry>,
}

/// One reverse-proxy rule in the v1 API (maps to [`Proxy`]).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct RouteEntry {
    pub name: Option<String>,
    pub host: Option<String>,
    pub source: String,
    pub target: Option<String>,
    pub targets: Option<Vec<String>>,
    pub strip_prefix: Option<String>,
}

impl RoutesDocument {
    /// Validates and converts the document into TOML-equivalent proxy definitions.
    pub fn into_proxy_defs(self) -> Result<Vec<Proxy>> {
        if self.version != 1 {
            bail!(
                "routes bootstrap: unsupported document version {} (expected 1)",
                self.version
            );
        }

        let mut defs = Vec::new();
        for app in self.apps {
            for route in app.routes {
                validate_route_source(&route.source, &app.id)?;
                if let Some(ref prefix) = route.strip_prefix {
                    if !prefix.starts_with('/') {
                        bail!(
                            "routes bootstrap: strip-prefix must start with '/' (app={}, source={})",
                            app.id,
                            route.source
                        );
                    }
                }
                defs.push(Proxy {
                    name: route.name,
                    host: route.host,
                    source: route.source,
                    target: route.target,
                    targets: route.targets,
                    strip_prefix: route.strip_prefix,
                });
            }
        }
        Ok(defs)
    }
}

fn validate_route_source(source: &str, app_id: &str) -> Result<()> {
    if !source.starts_with('/') {
        bail!(
            "routes bootstrap: source must start with '/' (app={app_id}): {source}"
        );
    }
    if source.contains("..") {
        bail!(
            "routes bootstrap: source must not contain '..' (app={app_id}): {source}"
        );
    }
    Ok(())
}
