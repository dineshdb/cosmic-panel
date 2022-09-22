// SPDX-License-Identifier: MPL-2.0-only

use std::{
    fs::File,
    io::{BufRead, BufReader},
    path::PathBuf,
};

use adw_user_colors_lib::NAME;
use anyhow::Result;
use cosmic_panel_config::{CosmicPanelBackground, CosmicPanelContainerConfig};
use launch_pad::ProcessManager;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use slog::{o, warn, Drain};
use smithay::reexports::calloop;
use tokio::{
    runtime,
    sync::mpsc,
};
use xdg_shell_wrapper::{run, shared_state::GlobalState};

mod space;
mod space_container;

fn get_color(path: &PathBuf) -> Option<[f32; 4]> {
    let file = match File::open(path) {
        Ok(f) => f,
        _ => return None,
    };

    let window_bg_color_pattern = "@define-color window_bg_color";
    if let Some(color) = BufReader::new(file)
        .lines()
        .filter_map(|l| l.ok())
        .find_map(|line| {
            line.rfind(window_bg_color_pattern)
                .and_then(|i| line.get(i + window_bg_color_pattern.len()..))
                .and_then(|color_str| {
                    csscolorparser::parse(&color_str.trim().replace(";", "")).ok()
                })
        })
    {
        return Some([
            color.r as f32,
            color.g as f32,
            color.b as f32,
            color.a as f32,
        ]);
    }
    None
}

fn main() -> Result<()> {
    let log = slog::Logger::root(
        slog_async::Async::default(slog_term::term_full().fuse()).fuse(),
        o!(),
    );

    let _guard = slog_scope::set_global_logger(log.clone());
    slog_stdlog::init().expect("Could not setup log backend");

    let arg = std::env::args().nth(1);
    let usage = "USAGE: cosmic-panel";
    let config = match arg.as_ref().map(|s| &s[..]) {
        Some(arg) if arg == "--help" || arg == "-h" => {
            println!("{}", usage);
            std::process::exit(1);
        }
        None => match cosmic_panel_config::CosmicPanelContainerConfig::load() {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    log.clone(),
                    "Falling back to default panel configuration: {}", e
                );
                CosmicPanelContainerConfig::default()
            }
        },
        _ => {
            println!("{}", usage);
            std::process::exit(1);
        }
    };
    
    let (applet_tx, mut applet_rx) = mpsc::channel(200);

    let mut space = space_container::SpaceContainer::new(config, log, applet_tx);

    let event_loop = calloop::EventLoop::try_new()?;
    if space.config
        .config_list
        .iter()
        .any(|c| matches!(c.background, CosmicPanelBackground::ThemeDefault(_)))
    {
        let (color_tx, color_rx) = calloop::channel::sync_channel(100);

        let path = xdg::BaseDirectories::with_prefix("gtk-4.0")
            .ok()
            .and_then(|xdg_dirs| xdg_dirs.find_config_file("cosmic.css"))
            .unwrap_or_else(|| "~/.config/gtk-4.0/cosmic.css".into());
        if let Ok(xdg_dirs) = xdg::BaseDirectories::with_prefix(NAME) {
            // initital send of color
            space.set_theme_window_color(get_color(&path).unwrap_or_else(|| [0.5, 0.5, 0.5, 0.5]));
                        // Automatically select the best implementation for your platform.
            // You can also access each implementation directly e.g. INotifyWatcher.
            let color_tx_clone = color_tx.clone();
            if let Ok(mut watcher) = RecommendedWatcher::new(
                move |res| {
                    if let Ok(e) = res {
                        let color_tx = color_tx_clone.clone();
                        let _ = color_tx.send(e);
                    }
                },
                notify::Config::default(),
            ) {
                for config_dir in xdg_dirs.get_config_dirs() {
                    let _ = watcher.watch(&config_dir, RecursiveMode::Recursive);
                }
                for data_dir in xdg_dirs.get_data_dirs() {
                    let _ = watcher.watch(&&data_dir.as_ref(), RecursiveMode::Recursive);
                }
        
                event_loop
                    .handle()
                    .insert_source(
                        color_rx,
                        move |e, _, state: &mut GlobalState<space_container::SpaceContainer>| {
                            match e {
                                calloop::channel::Event::Msg(e) => {
                                    match e.kind {
                                        // TODO only notify for changed data file if it is the active file
                                        notify::EventKind::Create(_)
                                        | notify::EventKind::Modify(_)
                                        | notify::EventKind::Remove(_) => {
                                            let _ = state.space.set_theme_window_color(get_color(&path).unwrap_or_else(|| [0.5, 0.5, 0.5, 0.5]));
                                        }
                                        _ => {}
                                    }
                                },
                                calloop::channel::Event::Closed => {}
                            };
                        },
                    )
                    .expect("failed to insert dbus event source");
    
                for data_dir in xdg_dirs.get_data_dirs() {
                    let _ = watcher.watch(&&data_dir.as_ref(), RecursiveMode::Recursive);
                }
            }
        }

        std::thread::spawn(move || -> anyhow::Result<()> {
            let rt = runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            rt.block_on(async {
                let process_manager = ProcessManager::new().await;
                let _ = process_manager.set_max_restarts(10);
                while let Some(process) = applet_rx.recv().await {
                    // TODO handle keys
                    let _ = process_manager.start(process).await;
                }
            });

            Ok(())
        });
    }

    run(
        space,
        event_loop,
    )?;
    Ok(())
}
