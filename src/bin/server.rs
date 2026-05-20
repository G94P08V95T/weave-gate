// SPDX-License-Identifier: MIT OR Apache-2.0
// This file is part of WeaveGate.
// WeaveGate — frontend gateway and static file server.
// Copyright (C) 2019-present Jose Quintana <joseluisq.net>

#![forbid(unsafe_code)]
#![deny(warnings)]
#![deny(rust_2018_idioms)]
#![deny(dead_code)]

#[cfg(all(target_env = "musl", target_pointer_width = "64"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use weavegate::{
    Result, Settings,
    settings::{Commands, cli::General},
};

fn main() -> Result {
    let opts = Settings::get(true)?;

    if opts.general.version {
        return weavegate::settings::cli_output::display_version();
    }

    if let Some(commands) = opts.general.commands {
        match commands {
            #[cfg(windows)]
            Commands::Install {} => {
                return weavegate::winservice::install_service(&opts.general.config_file);
            }
            #[cfg(windows)]
            Commands::Uninstall {} => {
                return weavegate::winservice::uninstall_service();
            }
            Commands::Generate {
                completions,
                man_pages,
                out_dir,
            } => {
                if completions || !man_pages {
                    let mut comp_dir = out_dir.clone();
                    comp_dir.push("completions");
                    clap_allgen::render_shell_completions::<General>(&comp_dir)?;
                    tracing::info!("wrote completions to {}", comp_dir.to_string_lossy());
                }
                if man_pages || !completions {
                    let mut man_dir = out_dir.clone();
                    man_dir.push("man");
                    clap_allgen::render_manpages::<General>(&man_dir)?;
                    tracing::info!("wrote man pages to {}", man_dir.to_string_lossy());
                }
                return Ok(());
            }
        }
    }

    #[cfg(windows)]
    if opts.general.windows_service {
        return weavegate::winservice::run_server_as_service();
    }

    // Run the server by default
    weavegate::Server::new(opts)?.run_standalone(None)?;

    Ok(())
}
