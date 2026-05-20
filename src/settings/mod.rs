// SPDX-License-Identifier: MIT OR Apache-2.0
// This file is part of WeaveGate.
// WeaveGate — frontend gateway and static file server.
// Copyright (C) 2019-present Jose Quintana <joseluisq.net>

//! Module that provides all settings of SWS.
//!

use clap::Parser;
use globset::{Glob, GlobBuilder, GlobMatcher};
use headers::HeaderMap;
use hyper::StatusCode;
use regex_lite::Regex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::{Context, Result, helpers, logger};

pub mod cli;
#[doc(hidden)]
pub mod cli_output;
pub mod file;

pub use cli::Commands;

use cli::General;

#[cfg(feature = "experimental")]
use self::file::MemoryCache;

use self::file::{RedirectsKind, Settings as FileSettings};

#[cfg(any(
    feature = "compression",
    feature = "compression-gzip",
    feature = "compression-brotli",
    feature = "compression-zstd",
    feature = "compression-deflate"
))]
pub use file::CompressionLevel;

/// The `headers` file options.
pub struct Headers {
    /// Source pattern glob matcher
    pub source: GlobMatcher,
    /// Map of custom HTTP headers
    pub headers: HeaderMap,
}

/// The `Rewrites` file options.
pub struct Rewrites {
    /// Source pattern Regex matcher
    pub source: Regex,
    /// A local file that must exist
    pub destination: String,
    /// Optional redirect type either 301 (Moved Permanently) or 302 (Found).
    pub redirect: Option<RedirectsKind>,
}

/// The `Redirects` file options.
pub struct Redirects {
    /// Optional host to match against an incoming URI host if specified
    pub host: Option<String>,
    /// Source pattern Regex matcher
    pub source: Regex,
    /// A local file that must exist
    pub destination: String,
    /// Redirection type either 301 (Moved Permanently) or 302 (Found)
    pub kind: StatusCode,
}

/// The `VirtualHosts` file options.
pub struct VirtualHosts {
    /// The value to check for in the "Host" header
    pub host: String,
    /// The root directory for this virtual host
    pub root: PathBuf,
}

#[cfg(feature = "proxy")]
/// Round-robin load balancer over upstream instances.
pub struct ProxyBalancer {
    /// Optional service name for logging.
    pub name: Option<String>,
    targets: Vec<hyper::Uri>,
    next: AtomicUsize,
}

#[cfg(feature = "proxy")]
impl ProxyBalancer {
    /// Creates a balancer from one or more validated upstream base URIs.
    pub fn new(name: Option<String>, targets: Vec<hyper::Uri>) -> Result<Arc<Self>> {
        if targets.is_empty() {
            bail!("proxy service must have at least one upstream target");
        }
        Ok(Arc::new(Self {
            name,
            targets,
            next: AtomicUsize::new(0),
        }))
    }

    /// Returns the next upstream base URI (round-robin).
    pub fn next(&self) -> &hyper::Uri {
        let index = self.next_index();
        &self.targets[index]
    }

    /// Returns the next upstream index (round-robin).
    pub fn next_index(&self) -> usize {
        let index = self.next.fetch_add(1, Ordering::Relaxed);
        index % self.targets.len()
    }

    /// Number of upstream instances.
    pub fn len(&self) -> usize {
        self.targets.len()
    }

    /// Comma-separated upstream URIs for logging.
    pub fn targets_display(&self) -> String {
        self.targets
            .iter()
            .map(|u| u.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(feature = "proxy")]
/// Precomputed URI parts for an upstream base (avoids per-request format/parse).
#[derive(Clone)]
pub struct UpstreamUriTemplate {
    /// `scheme://authority` without trailing slash.
    pub origin: String,
    /// Base path on upstream (may be empty).
    pub base_path: String,
    /// Optional strip prefix from incoming requests.
    pub strip_prefix: Option<String>,
}

#[cfg(feature = "proxy")]
impl UpstreamUriTemplate {
    /// Builds the upstream request URI for an incoming path-and-query.
    pub fn build(&self, req_uri: &hyper::Uri) -> Result<hyper::Uri> {
        crate::proxy::build_upstream_uri_from_template(self, req_uri)
    }
}

#[cfg(feature = "proxy")]
/// Compiled reverse proxy rule.
pub struct ProxyRule {
    /// Optional host filter (without port).
    pub host: Option<String>,
    /// Path glob matcher.
    pub source: GlobMatcher,
    /// Upstream instances with round-robin selection.
    pub balancer: Arc<ProxyBalancer>,
    /// Optional path prefix stripped before forwarding.
    pub strip_prefix: Option<String>,
    /// Per-upstream URI templates aligned with balancer targets.
    pub uri_templates: Vec<UpstreamUriTemplate>,
}

#[cfg(feature = "proxy")]
/// TLS trust store for HTTPS upstream and bootstrap clients.
#[derive(Clone, Debug, Default)]
pub struct ProxyTlsSettings {
    /// Extra CA certificates (PEM), e.g. mkcert local root.
    pub ca_files: Vec<PathBuf>,
    /// Include Mozilla/WebPKI public roots (default `true`).
    pub use_webpki_roots: bool,
}

#[cfg(feature = "proxy")]
impl ProxyTlsSettings {
    /// Builds runtime TLS settings from file `[advanced.proxy-tls]`.
    pub fn from_file(file: &file::ProxyTls) -> Result<Self> {
        let mut ca_files = Vec::new();
        if let Some(ref path) = file.ca_file {
            ca_files.push(expand_tls_path(path)?);
        }
        if let Some(ref paths) = file.ca_files {
            for path in paths {
                ca_files.push(expand_tls_path(path)?);
            }
        }
        let use_webpki_roots = !file.disable_webpki_roots.unwrap_or(false);
        if !use_webpki_roots && ca_files.is_empty() {
            bail!(
                "proxy-tls: disable-webpki-roots requires ca-file or ca-files (e.g. mkcert rootCA.pem)"
            );
        }
        for path in &ca_files {
            if !path.is_file() {
                bail!(
                    "proxy-tls: CA file not found or not a regular file: {}",
                    path.display()
                );
            }
        }
        Ok(Self {
            ca_files,
            use_webpki_roots,
        })
    }
}

#[cfg(feature = "proxy")]
fn expand_tls_path(path: &Path) -> Result<PathBuf> {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .with_context(|| "proxy-tls: cannot expand '~/' (HOME not set)")?;
        return Ok(home.join(rest));
    }
    if s == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .with_context(|| "proxy-tls: cannot expand '~' (HOME not set)");
    }
    Ok(path.to_path_buf())
}

#[cfg(feature = "proxy")]
/// Runtime options for the reverse proxy client and handler ordering.
#[derive(Clone, Debug)]
pub struct ProxySettings {
    /// Max idle HTTP connections kept per upstream host in the proxy pool.
    pub pool_max_idle_per_host: usize,
    /// Seconds before an idle pooled upstream connection is closed.
    pub pool_idle_timeout_secs: u64,
    /// When true, evaluate proxy rules before redirects and rewrites.
    pub proxy_first: bool,
    /// `None` = unlimited concurrent protocol-upgrade tunnels.
    pub max_upgrade_tunnels: Option<usize>,
    /// TLS trust for HTTPS upstream / bootstrap.
    pub tls: ProxyTlsSettings,
}

#[cfg(feature = "proxy")]
impl Default for ProxySettings {
    fn default() -> Self {
        Self {
            pool_max_idle_per_host: 32,
            pool_idle_timeout_secs: 90,
            proxy_first: false,
            max_upgrade_tunnels: None,
            tls: ProxyTlsSettings::default(),
        }
    }
}

#[cfg(feature = "proxy")]
/// Builds URI templates for compiled proxy rules (also used in tests).
pub fn upstream_uri_template(
    upstream_base: &hyper::Uri,
    strip_prefix: Option<&str>,
) -> Result<UpstreamUriTemplate> {
    let authority = upstream_base
        .authority()
        .ok_or_else(|| anyhow::anyhow!("upstream URI has no authority"))?;
    let scheme = upstream_base
        .scheme_str()
        .ok_or_else(|| anyhow::anyhow!("upstream URI has no scheme"))?;
    let mut base_path = upstream_base.path().to_string();
    if base_path.is_empty() {
        base_path = "/".to_string();
    }
    Ok(UpstreamUriTemplate {
        origin: format!("{scheme}://{authority}"),
        base_path,
        strip_prefix: strip_prefix.map(str::to_string),
    })
}

/// The `advanced` file options.
pub struct Advanced {
    /// Headers list.
    pub headers: Option<Vec<Headers>>,
    /// Rewrites list.
    pub rewrites: Option<Vec<Rewrites>>,
    /// Redirects list.
    pub redirects: Option<Vec<Redirects>>,
    /// Name-based virtual hosting
    pub virtual_hosts: Option<Vec<VirtualHosts>>,
    #[cfg(feature = "proxy")]
    /// Reverse proxy rules (compiled at server startup).
    pub proxies: Option<Vec<ProxyRule>>,
    #[cfg(feature = "proxy")]
    /// Proxy rules from TOML before bootstrap merge/compile.
    pub proxy_defs: Option<Vec<file::Proxy>>,
    #[cfg(feature = "proxy")]
    /// Optional control-plane bootstrap configuration.
    pub routes_bootstrap: Option<file::RoutesBootstrap>,
    #[cfg(feature = "proxy")]
    /// Proxy client pool and handler ordering.
    pub proxy_settings: ProxySettings,
    #[cfg(feature = "experimental")]
    /// In-memory cache feature (experimental).
    pub memory_cache: Option<MemoryCache>,
}

impl Default for Advanced {
    fn default() -> Self {
        Self {
            headers: None,
            rewrites: None,
            redirects: None,
            virtual_hosts: None,
            #[cfg(feature = "proxy")]
            proxies: None,
            #[cfg(feature = "proxy")]
            proxy_defs: None,
            #[cfg(feature = "proxy")]
            routes_bootstrap: None,
            #[cfg(feature = "proxy")]
            proxy_settings: ProxySettings::default(),
            #[cfg(feature = "experimental")]
            memory_cache: None,
        }
    }
}

/// The full server CLI and File options.
pub struct Settings {
    /// General server options
    pub general: General,
    /// Advanced server options
    pub advanced: Option<Advanced>,
}

impl Settings {
    /// Reads CLI/Env and config file options returning the server settings.
    /// It also takes care to initialize the logging system with its level
    /// once the `general` settings are determined.
    pub fn get(log_init: bool) -> Result<Settings> {
        Self::parse_from(log_init, None)
    }

    /// Reads CLI/Env and config file options returning the server settings
    /// without parsing arguments useful for testing.
    pub fn get_unparsed(log_init: bool, args: &[&str]) -> Result<Settings> {
        Self::parse_from(log_init, Some(args))
    }

    fn parse_from(log_init: bool, args: Option<&[&str]>) -> Result<Settings> {
        let opts = match args {
            Some(v) => General::parse_from(v),
            None => General::parse(),
        };

        // Define the general CLI/file options
        let version = opts.version;
        let mut host = opts.host;
        let mut port = opts.port;
        let mut root = opts.root;
        let mut log_level = opts.log_level;
        let mut log_with_ansi = opts.log_with_ansi;
        let mut config_file = opts.config_file.clone();
        let mut cache_control_headers = opts.cache_control_headers;

        #[cfg(any(
            feature = "compression",
            feature = "compression-gzip",
            feature = "compression-brotli",
            feature = "compression-zstd",
            feature = "compression-deflate"
        ))]
        let mut compression = opts.compression;
        #[cfg(any(
            feature = "compression",
            feature = "compression-gzip",
            feature = "compression-brotli",
            feature = "compression-zstd",
            feature = "compression-deflate"
        ))]
        let mut compression_level = opts.compression_level;

        let mut compression_static = opts.compression_static;

        let mut page404 = opts.page404;
        let mut page50x = opts.page50x;

        #[cfg(feature = "http2")]
        let mut http2 = opts.http2;
        #[cfg(feature = "http2")]
        let mut http2_tls_cert = opts.http2_tls_cert;
        #[cfg(feature = "http2")]
        let mut http2_tls_key = opts.http2_tls_key;
        #[cfg(feature = "http2")]
        let mut https_redirect = opts.https_redirect;
        #[cfg(feature = "http2")]
        let mut https_redirect_host = opts.https_redirect_host;
        #[cfg(feature = "http2")]
        let mut https_redirect_from_port = opts.https_redirect_from_port;
        #[cfg(feature = "http2")]
        let mut https_redirect_from_hosts = opts.https_redirect_from_hosts;

        let mut security_headers = opts.security_headers;
        let mut cors_allow_origins = opts.cors_allow_origins;
        let mut cors_allow_headers = opts.cors_allow_headers;
        let mut cors_expose_headers = opts.cors_expose_headers;

        #[cfg(feature = "directory-listing")]
        let mut directory_listing = opts.directory_listing;
        #[cfg(feature = "directory-listing")]
        let mut directory_listing_order = opts.directory_listing_order;
        #[cfg(feature = "directory-listing")]
        let mut directory_listing_format = opts.directory_listing_format;

        #[cfg(feature = "directory-listing-download")]
        let mut directory_listing_download = opts.directory_listing_download;

        #[cfg(feature = "basic-auth")]
        let mut basic_auth = opts.basic_auth;

        let mut fd = opts.fd;
        let mut threads_multiplier = opts.threads_multiplier;
        let mut max_blocking_threads = opts.max_blocking_threads;
        let mut grace_period = opts.grace_period;

        #[cfg(feature = "fallback-page")]
        let mut page_fallback = opts.page_fallback;

        let mut log_remote_address = opts.log_remote_address;
        let mut log_x_real_ip = opts.log_x_real_ip;
        let mut log_forwarded_for = opts.log_forwarded_for;
        let mut trusted_proxies = opts.trusted_proxies;
        let mut redirect_trailing_slash = opts.redirect_trailing_slash;
        let mut ignore_hidden_files = opts.ignore_hidden_files;
        let mut disable_symlinks = opts.disable_symlinks;
        let mut accept_markdown = opts.accept_markdown;
        let mut default_text_charset = opts.text_charset;
        let mut index_files = opts.index_files;
        let mut health = opts.health;

        #[cfg(feature = "metrics")]
        let mut metrics = opts.metrics;

        let mut maintenance_mode = opts.maintenance_mode;
        let mut maintenance_mode_status = opts.maintenance_mode_status;
        let mut maintenance_mode_file = opts.maintenance_mode_file;

        // Windows-only options
        #[cfg(windows)]
        let mut windows_service = opts.windows_service;

        // Define the advanced file options
        let mut settings_advanced: Option<Advanced> = None;

        let to_use_config_file = if Path::new("./weavegate.toml").is_file() {
            PathBuf::from("./weavegate.toml")
        } else if Path::new("./sws.toml").is_file() {
            eprintln!(
                "Deprecated: 'sws.toml' found, rename it to 'weavegate.toml' for future releases"
            );
            PathBuf::from("./sws.toml")
        } else if Path::new("./edgegate.toml").is_file() {
            eprintln!(
                "Deprecated: 'edgegate.toml' found, rename it to 'weavegate.toml' for future releases"
            );
            PathBuf::from("./edgegate.toml")
        } else if Path::new("./config.toml").is_file() {
            eprintln!(
                "Deprecated: 'config.toml' found, rename it to 'weavegate.toml' for future releases"
            );
            PathBuf::from("./config.toml")
        } else {
            opts.config_file.clone()
        };

        if let Some((settings, config_file_resolved)) = read_file_settings(&to_use_config_file)? {
            config_file = config_file_resolved;

            // File-based "general" options
            let has_general_settings = settings.general.is_some();
            if has_general_settings {
                let general = settings.general.unwrap();

                if let Some(v) = general.host {
                    host = v
                }
                if let Some(v) = general.port {
                    port = v
                }
                if let Some(v) = general.root {
                    root = v
                }
                if let Some(ref v) = general.log_level {
                    log_level = v.name().to_lowercase();
                }
                if let Some(v) = general.log_with_ansi {
                    log_with_ansi = v;
                }
                if let Some(v) = general.cache_control_headers {
                    cache_control_headers = v
                }
                #[cfg(any(
                    feature = "compression",
                    feature = "compression-gzip",
                    feature = "compression-brotli",
                    feature = "compression-zstd",
                    feature = "compression-deflate"
                ))]
                if let Some(v) = general.compression {
                    compression = v
                }
                #[cfg(any(
                    feature = "compression",
                    feature = "compression-gzip",
                    feature = "compression-brotli",
                    feature = "compression-zstd",
                    feature = "compression-deflate"
                ))]
                if let Some(v) = general.compression_level {
                    compression_level = v
                }
                if let Some(v) = general.compression_static {
                    compression_static = v
                }
                if let Some(v) = general.page404 {
                    page404 = v
                }
                if let Some(v) = general.page50x {
                    page50x = v
                }
                #[cfg(feature = "http2")]
                if let Some(v) = general.http2 {
                    http2 = v
                }
                #[cfg(feature = "http2")]
                if let Some(v) = general.http2_tls_cert {
                    http2_tls_cert = Some(v)
                }
                #[cfg(feature = "http2")]
                if let Some(v) = general.http2_tls_key {
                    http2_tls_key = Some(v)
                }
                #[cfg(feature = "http2")]
                if let Some(v) = general.https_redirect {
                    https_redirect = v
                }
                #[cfg(feature = "http2")]
                if let Some(v) = general.https_redirect_host {
                    https_redirect_host = v
                }
                #[cfg(feature = "http2")]
                if let Some(v) = general.https_redirect_from_port {
                    https_redirect_from_port = v
                }
                #[cfg(feature = "http2")]
                if let Some(v) = general.https_redirect_from_hosts {
                    https_redirect_from_hosts = v
                }
                #[cfg(feature = "http2")]
                match general.security_headers {
                    Some(v) => security_headers = v,
                    _ => {
                        if http2 {
                            security_headers = true;
                        }
                    }
                }
                #[cfg(not(feature = "http2"))]
                if let Some(v) = general.security_headers {
                    security_headers = v
                }
                if let Some(ref v) = general.cors_allow_origins {
                    v.clone_into(&mut cors_allow_origins)
                }
                if let Some(ref v) = general.cors_allow_headers {
                    v.clone_into(&mut cors_allow_headers)
                }
                if let Some(ref v) = general.cors_expose_headers {
                    v.clone_into(&mut cors_expose_headers)
                }
                #[cfg(feature = "directory-listing")]
                if let Some(v) = general.directory_listing {
                    directory_listing = v
                }
                #[cfg(feature = "directory-listing")]
                if let Some(v) = general.directory_listing_order {
                    directory_listing_order = v
                }
                #[cfg(feature = "directory-listing")]
                if let Some(v) = general.directory_listing_format {
                    directory_listing_format = v
                }
                #[cfg(feature = "directory-listing-download")]
                if let Some(v) = general.directory_listing_download {
                    directory_listing_download = v
                }
                #[cfg(feature = "basic-auth")]
                if let Some(ref v) = general.basic_auth {
                    v.clone_into(&mut basic_auth)
                }
                if let Some(v) = general.fd {
                    fd = Some(v)
                }
                if let Some(v) = general.threads_multiplier {
                    threads_multiplier = v
                }
                if let Some(v) = general.max_blocking_threads {
                    max_blocking_threads = v
                }
                if let Some(v) = general.grace_period {
                    grace_period = v
                }
                #[cfg(feature = "fallback-page")]
                if let Some(v) = general.page_fallback {
                    page_fallback = v
                }
                if let Some(v) = general.log_remote_address {
                    log_remote_address = v
                }
                if let Some(v) = general.log_x_real_ip {
                    log_x_real_ip = v
                }
                if let Some(v) = general.log_forwarded_for {
                    log_forwarded_for = v
                }
                if let Some(v) = general.trusted_proxies {
                    trusted_proxies = v
                }
                if let Some(v) = general.redirect_trailing_slash {
                    redirect_trailing_slash = v
                }
                if let Some(v) = general.ignore_hidden_files {
                    ignore_hidden_files = v
                }
                if let Some(v) = general.disable_symlinks {
                    disable_symlinks = v
                }
                if let Some(v) = general.health {
                    health = v
                }
                if let Some(v) = general.accept_markdown {
                    accept_markdown = v
                }
                if let Some(v) = general.text_charset {
                    default_text_charset = v
                }
                #[cfg(feature = "metrics")]
                if let Some(v) = general.metrics {
                    metrics = v
                }
                if let Some(v) = general.index_files {
                    index_files = v
                }
                if let Some(v) = general.maintenance_mode {
                    maintenance_mode = v
                }
                if let Some(v) = general.maintenance_mode_status {
                    maintenance_mode_status =
                        StatusCode::from_u16(v).with_context(|| "invalid HTTP status code")?
                }
                if let Some(v) = general.maintenance_mode_file {
                    maintenance_mode_file = v
                }

                // Windows-only options
                #[cfg(windows)]
                if let Some(v) = general.windows_service {
                    windows_service = v
                }
            }

            // Logging system initialization in config file context
            if log_init {
                logger::init(log_level.as_str(), log_with_ansi)?;
            }

            tracing::debug!("config file read successfully");
            tracing::debug!("config file path provided: {}", opts.config_file.display());
            tracing::debug!("config file path resolved: {}", config_file.display());

            if !has_general_settings {
                tracing::warn!(
                    "config file empty or no `general` settings found, using default values"
                );
            }

            // File-based "advanced" options
            if let Some(advanced) = settings.advanced {
                // 1. Custom HTTP headers assignment
                let headers_entries = match advanced.headers {
                    Some(headers_entries) => {
                        let mut headers_vec: Vec<Headers> = Vec::new();

                        // Compile a glob pattern for each header sources entry
                        for headers_entry in headers_entries.iter() {
                            let source = Glob::new(&headers_entry.source)
                                .with_context(|| {
                                    format!(
                                        "can not compile glob pattern for header source: {}",
                                        &headers_entry.source
                                    )
                                })?
                                .compile_matcher();

                            headers_vec.push(Headers {
                                source,
                                headers: headers_entry.headers.to_owned(),
                            });
                        }
                        Some(headers_vec)
                    }
                    _ => None,
                };

                // 2. Rewrites assignment
                let rewrites_entries = match advanced.rewrites {
                    Some(rewrites_entries) => {
                        let mut rewrites_vec: Vec<Rewrites> = Vec::new();

                        // Compile a glob pattern for each rewrite sources entry
                        for rewrites_entry in rewrites_entries.iter() {
                            let source = GlobBuilder::new(&rewrites_entry.source)
                                .literal_separator(true)
                                .build()
                                .with_context(|| {
                                    format!(
                                        "can not compile glob pattern for rewrite source: {}",
                                        &rewrites_entry.source
                                    )
                                })?
                                .compile_matcher();

                            let pattern = source
                                .glob()
                                .regex()
                                .trim_start_matches("(?-u)")
                                .replace("?:.*", ".*")
                                .replace("?:", "")
                                .replace(".*.*", ".*")
                                .to_owned();
                            tracing::debug!(
                                "url rewrites glob pattern: {}",
                                &rewrites_entry.source
                            );
                            tracing::debug!("url rewrites regex equivalent: {}", pattern);

                            let source = Regex::new(&pattern).with_context(|| {
                                    format!(
                                        "can not compile regex pattern equivalent for rewrite source: {}",
                                        &pattern
                                    )
                                })?;

                            rewrites_vec.push(Rewrites {
                                source,
                                destination: rewrites_entry.destination.to_owned(),
                                redirect: rewrites_entry.redirect.to_owned(),
                            });
                        }
                        Some(rewrites_vec)
                    }
                    _ => None,
                };

                // 3. Redirects assignment
                let redirects_entries = match advanced.redirects {
                    Some(redirects_entries) => {
                        let mut redirects_vec: Vec<Redirects> = Vec::new();

                        // Compile a glob pattern for each redirect sources entry
                        for redirects_entry in redirects_entries.iter() {
                            let source = GlobBuilder::new(&redirects_entry.source)
                                .literal_separator(true)
                                .build()
                                .with_context(|| {
                                    format!(
                                        "can not compile glob pattern for redirect source: {}",
                                        &redirects_entry.source
                                    )
                                })?
                                .compile_matcher();

                            let pattern = source
                                .glob()
                                .regex()
                                .trim_start_matches("(?-u)")
                                .replace("?:.*", ".*")
                                .replace("?:", "")
                                .replace(".*.*", ".*")
                                .to_owned();
                            tracing::debug!(
                                "url redirects glob pattern: {}",
                                &redirects_entry.source
                            );
                            tracing::debug!("url redirects regex equivalent: {}", pattern);

                            let source = Regex::new(&pattern).with_context(|| {
                                    format!(
                                        "can not compile regex pattern equivalent for redirect source: {}",
                                        &pattern
                                    )
                                })?;

                            let status_code = redirects_entry.kind.to_owned() as u16;
                            redirects_vec.push(Redirects {
                                host: redirects_entry.host.to_owned(),
                                source,
                                destination: redirects_entry.destination.to_owned(),
                                kind: StatusCode::from_u16(status_code).with_context(|| {
                                    format!("invalid redirect status code: {status_code}")
                                })?,
                            });
                        }
                        Some(redirects_vec)
                    }
                    _ => None,
                };

                #[cfg(feature = "proxy")]
                let proxy_defs_entries = advanced.proxies.clone();
                #[cfg(feature = "proxy")]
                let routes_bootstrap_entries = advanced.routes_bootstrap.clone();

                #[cfg(feature = "proxy")]
                let proxy_tls = match advanced.proxy_tls.as_ref() {
                    Some(tls) => ProxyTlsSettings::from_file(tls)?,
                    None => ProxyTlsSettings::default(),
                };
                #[cfg(feature = "proxy")]
                if !proxy_tls.ca_files.is_empty() {
                    tracing::info!(
                        "proxy-tls: {} custom CA file(s), webpki_roots={}",
                        proxy_tls.ca_files.len(),
                        proxy_tls.use_webpki_roots
                    );
                }

                #[cfg(feature = "proxy")]
                let proxy_settings = ProxySettings {
                    pool_max_idle_per_host: advanced
                        .proxy_pool_max_idle_per_host
                        .unwrap_or(32),
                    pool_idle_timeout_secs: advanced
                        .proxy_pool_idle_timeout_secs
                        .unwrap_or(90),
                    proxy_first: advanced.proxy_first.unwrap_or(false),
                    max_upgrade_tunnels: advanced.max_upgrade_tunnels.and_then(|n| {
                        if n == 0 { None } else { Some(n) }
                    }),
                    tls: proxy_tls,
                };

                // 4. Virtual hosts assignment
                let vhosts_entries = match advanced.virtual_hosts {
                    Some(vhosts_entries) => {
                        let mut vhosts_vec: Vec<VirtualHosts> = Vec::new();

                        for vhosts_entry in vhosts_entries.iter() {
                            if let Some(root) = vhosts_entry.root.to_owned() {
                                // Make sure path is valid
                                let root_dir = helpers::get_valid_dirpath(&root)
                                    .with_context(|| "root directory for virtual host was not found or inaccessible")?;
                                tracing::debug!(
                                    "added virtual host: {} -> {}",
                                    vhosts_entry.host,
                                    root_dir.display()
                                );
                                vhosts_vec.push(VirtualHosts {
                                    host: vhosts_entry.host.to_owned(),
                                    root: root_dir,
                                });
                            }
                        }
                        Some(vhosts_vec)
                    }
                    _ => None,
                };

                settings_advanced = Some(Advanced {
                    headers: headers_entries,
                    rewrites: rewrites_entries,
                    redirects: redirects_entries,
                    virtual_hosts: vhosts_entries,
                    #[cfg(feature = "proxy")]
                    proxies: None,
                    #[cfg(feature = "proxy")]
                    proxy_defs: proxy_defs_entries,
                    #[cfg(feature = "proxy")]
                    routes_bootstrap: routes_bootstrap_entries,
                    #[cfg(feature = "proxy")]
                    proxy_settings,
                    #[cfg(feature = "experimental")]
                    memory_cache: advanced.memory_cache,
                });
            }
        } else if log_init {
            // Logging system initialization on demand
            logger::init(log_level.as_str(), log_with_ansi)?;
        }

        Ok(Settings {
            general: General {
                version,
                host,
                port,
                root,
                log_level,
                log_with_ansi,
                config_file,
                cache_control_headers,
                #[cfg(any(
                    feature = "compression",
                    feature = "compression-gzip",
                    feature = "compression-brotli",
                    feature = "compression-zstd",
                    feature = "compression-deflate"
                ))]
                compression,
                #[cfg(any(
                    feature = "compression",
                    feature = "compression-gzip",
                    feature = "compression-brotli",
                    feature = "compression-zstd",
                    feature = "compression-deflate"
                ))]
                compression_level,
                compression_static,
                page404,
                page50x,
                #[cfg(feature = "http2")]
                http2,
                #[cfg(feature = "http2")]
                http2_tls_cert,
                #[cfg(feature = "http2")]
                http2_tls_key,
                #[cfg(feature = "http2")]
                https_redirect,
                #[cfg(feature = "http2")]
                https_redirect_host,
                #[cfg(feature = "http2")]
                https_redirect_from_port,
                #[cfg(feature = "http2")]
                https_redirect_from_hosts,
                security_headers,
                cors_allow_origins,
                cors_allow_headers,
                cors_expose_headers,
                #[cfg(feature = "directory-listing")]
                directory_listing,
                #[cfg(feature = "directory-listing")]
                directory_listing_order,
                #[cfg(feature = "directory-listing")]
                directory_listing_format,
                #[cfg(feature = "directory-listing-download")]
                directory_listing_download,
                #[cfg(feature = "basic-auth")]
                basic_auth,
                fd,
                threads_multiplier,
                max_blocking_threads,
                grace_period,
                #[cfg(feature = "fallback-page")]
                page_fallback,
                log_remote_address,
                log_x_real_ip,
                log_forwarded_for,
                trusted_proxies,
                redirect_trailing_slash,
                ignore_hidden_files,
                disable_symlinks,
                accept_markdown,
                text_charset: default_text_charset,
                index_files,
                health,
                #[cfg(feature = "metrics")]
                metrics,
                maintenance_mode,
                maintenance_mode_status,
                maintenance_mode_file,

                // Windows-only options and commands
                #[cfg(windows)]
                windows_service,
                commands: opts.commands,
            },
            advanced: settings_advanced,
        })
    }
}

fn read_file_settings(config_file: &Path) -> Result<Option<(FileSettings, PathBuf)>> {
    if config_file.is_file() {
        let file_path_resolved = config_file
            .canonicalize()
            .with_context(|| "unable to resolve toml config file path")?;

        let settings = FileSettings::read(&file_path_resolved).with_context(
            || "unable to read toml config file because has invalid format or unsupported options",
        )?;

        return Ok(Some((settings, file_path_resolved)));
    }
    Ok(None)
}
