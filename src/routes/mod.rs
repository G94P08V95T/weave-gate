// SPDX-License-Identifier: MIT OR Apache-2.0
// This file is part of WeaveGate.
// WeaveGate — frontend gateway and static file server.
// Copyright (C) 2019-present Jose Quintana <joseluisq.net>

//! Control-plane routes bootstrap (v1 JSON contract).

mod bootstrap;
mod document;

pub use bootstrap::fetch_routes_defs;
pub use document::RoutesDocument;
