// SPDX-License-Identifier: MIT OR Apache-2.0
// This file is part of WeaveGate.
// WeaveGate — frontend gateway and static file server.
// Copyright (C) 2019-present Jose Quintana <joseluisq.net>

use globset::Glob;

use crate::settings::file::Proxy;
use crate::settings::{ProxyBalancer, ProxyRule, UpstreamUriTemplate, upstream_uri_template};
use crate::{Context, Result};

/// Compiles TOML/API proxy definitions into runtime [`ProxyRule`] values.
pub fn compile_proxy_rules(proxy_entries: Vec<Proxy>) -> Result<Vec<ProxyRule>> {
    let mut proxies_vec = Vec::with_capacity(proxy_entries.len());

    for proxy_entry in proxy_entries.iter() {
        let source = Glob::new(&proxy_entry.source)
            .with_context(|| {
                format!(
                    "can not compile glob pattern for proxy source: {}",
                    &proxy_entry.source
                )
            })?
            .compile_matcher();

        if let Some(ref prefix) = proxy_entry.strip_prefix {
            if !prefix.starts_with('/') {
                bail!("proxy strip-prefix must start with '/': {prefix}");
            }
        }

        let upstreams = compile_proxy_targets(proxy_entry)?;
        let uri_templates: Vec<UpstreamUriTemplate> = upstreams
            .iter()
            .map(|u| upstream_uri_template(u, proxy_entry.strip_prefix.as_deref()))
            .collect::<Result<Vec<_>>>()?;
        let balancer = ProxyBalancer::new(proxy_entry.name.clone(), upstreams)?;

        let service = proxy_entry
            .name
            .as_deref()
            .unwrap_or(&proxy_entry.source);
        let lb_note = if balancer.len() > 1 {
            format!(" load-balance=round-robin instances={}", balancer.len())
        } else {
            String::new()
        };

        tracing::info!(
            "proxy rule: service={service} source={} targets=[{}]{lb_note}{}",
            proxy_entry.source,
            balancer.targets_display(),
            proxy_entry
                .strip_prefix
                .as_ref()
                .map(|p| format!(" strip-prefix={p}"))
                .unwrap_or_default()
        );

        proxies_vec.push(ProxyRule {
            host: proxy_entry.host.clone(),
            source,
            balancer,
            strip_prefix: proxy_entry.strip_prefix.clone(),
            uri_templates,
        });
    }

    Ok(proxies_vec)
}

fn compile_proxy_targets(entry: &Proxy) -> Result<Vec<hyper::Uri>> {
    let has_target = entry
        .target
        .as_ref()
        .is_some_and(|t| !t.trim().is_empty());
    let has_targets = entry
        .targets
        .as_ref()
        .is_some_and(|ts| !ts.is_empty());

    match (has_target, has_targets) {
        (true, true) => bail!(
            "proxy rule '{}' must use either `target` or `targets`, not both",
            entry.source
        ),
        (false, false) => bail!(
            "proxy rule '{}' must define `target` or `targets`",
            entry.source
        ),
        (true, false) => {
            let url = entry.target.as_ref().expect("checked above");
            Ok(vec![parse_proxy_target_url(url, &entry.source)?])
        }
        (false, true) => {
            let list = entry.targets.as_ref().expect("checked above");
            list.iter()
                .enumerate()
                .map(|(i, url)| {
                    parse_proxy_target_url(url, &format!("{}#{}", entry.source, i))
                })
                .collect()
        }
    }
}

fn parse_proxy_target_url(url: &str, context: &str) -> Result<hyper::Uri> {
    let target = url.trim().parse::<hyper::Uri>().with_context(|| {
        format!("invalid proxy target URL ({context}): {url}")
    })?;

    if target.scheme().is_none() {
        bail!(
            "proxy target must include a scheme (http:// or https://) ({context}): {url}"
        );
    }
    if target.authority().is_none() {
        bail!("proxy target must include a host ({context}): {url}");
    }

    Ok(target)
}
