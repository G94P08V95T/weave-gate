// SPDX-License-Identifier: MIT OR Apache-2.0
// This file is part of WeaveGate.
// WeaveGate — frontend gateway and static file server.
// Copyright (C) 2019-present Jose Quintana <joseluisq.net>

use crate::settings::file::Proxy;

/// Sorts proxy definitions so more specific globs match first in [`crate::proxy::find_rule`].
///
/// Longer `source` paths win; ties broken by fewer `*` wildcards.
pub fn sort_proxy_defs_by_specificity(defs: &mut [Proxy]) {
    defs.sort_by(|a, b| {
        let key_a = specificity_key(&a.source);
        let key_b = specificity_key(&b.source);
        key_b.cmp(&key_a)
    });
}

fn specificity_key(source: &str) -> (usize, usize) {
    let wildcards = source.matches('*').count();
    (source.len(), wildcards)
}
