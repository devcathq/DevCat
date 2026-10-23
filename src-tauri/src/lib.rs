// The CLI build (no `gui` feature) leaves the window-only commands unused.
#![cfg_attr(not(feature = "gui"), allow(dead_code, unused_imports))]

mod agent;
mod agent_tools;
mod ai;
mod bookmarks;
mod claude;
mod cli;
mod commands;
mod crashlog;
mod db;
#[cfg(debug_assertions)]
#[doc(hidden)]
pub mod devbridge;
mod error;
mod events;
mod fonts;
mod gitx;
mod history;
mod infra;
mod keep_awake;
mod models;
mod network;
mod notes;
mod notify;
#[cfg(feature = "gui")]
mod opened;
mod projects;
mod remote;
mod rt;
#[cfg(debug_assertions)]
#[doc(hidden)]
pub mod remotedemo;
mod secrets;
mod state;
mod store;
#[cfg(feature = "gui")]
mod updater;
#[cfg(feature = "gui")]
mod windows;

/// DevCat run by `ssh` as SSH_ASKPASS for a session's saved password (None: not that).
pub fn askpass_main() -> Option<i32> {
    claude::remote::askpass_main()
}

/// `devcat --vpn-helper <command> …`: the passwordless VPN helper (no Tauri, no window).
/// Returns the process exit code.
pub fn vpn_helper_main(args: &[String]) -> i32 {
    network::helper::cli_main(args)
}

/// Why this process must not open the app window (it is the root VPN helper), if so.
pub fn gui_refusal() -> Option<String> {
    network::helper::gui_refusal()
}

/// `devcat serve …` / `devcat help` …: DevCat without a window (see cli.rs), or None when the
/// arguments aren't a CLI command (start the app).
pub fn cli_main(args: &[String]) -> Option<i32> {
    cli::main(args)
}

fn init_process() {
    crashlog::install();
    // Both the `ring` and `aws-lc-rs` rustls backends end up in the dependency tree,
    // so rustls can't pick one automatically; choose it once for every TLS client.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

/// Everything behind the commands, the same with or without a window. Call `events::set_app`
/// first when there is one.
fn init_backend(dir: std::path::PathBuf) {
    network::init(dir.clone());
    ai::init(dir.clone());
    agent::init(dir.clone());
    state::init(dir.clone());
    network::start();
    notify::init(dir.clone());
    remote::init(dir);
}

/// Stop what must not outlive DevCat (VPNs, the remote server, agent runs, keep-awake).
fn shutdown_backend() {
    network::shutdown();
    claude::ssh_forward::stop_all();
    crate::rt::block_on(remote::shutdown());
    claude::runner::shutdown_all();
    agent::shutdown_all();
    keep_awake::shutdown();
}

/// The app (windows). Not in the CLI build (`--no-default-features`).
#[cfg(feature = "gui")]
pub fn run() {
    use tauri::Manager;
    init_process();

    tauri::Builder::default()
        // First: a second launch (e.g. double-clicking a .ovpn) hands its arguments to this one.
        .plugin(tauri_plugin_single_instance::init(|app, argv, cwd| {
            opened::accept(app, opened::from_args(&argv, Some(std::path::Path::new(&cwd))));
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let dir = app.path().app_data_dir().expect("no app data dir");
            events::set_app(app.handle().clone());
            init_backend(dir);
            #[cfg(target_os = "macos")]
            windows::install_menu(app.handle())?;
            // Windows / Linux: files DevCat was launched with.
            opened::accept(app.handle(), opened::from_args(&std::env::args().collect::<Vec<_>>(), None));
            Ok(())
        })
        .on_window_event(|window, event| {
            // A closed window takes its terminals, Claude runs and k8s streams with it.
            if let tauri::WindowEvent::Destroyed = event {
                windows::on_destroyed(window.label());
            }
        })
        .invoke_handler(tauri::generate_handler![
            // keep awake
            keep_awake::keep_awake_status,
            fonts::fonts_list,
            keep_awake::keep_awake_set,
            // updates
            updater::update_check,
            updater::update_install,
            // windows
            windows::open_window,
            opened::take_opened_files,
            // connections
            commands::connections::list_connections,
            commands::connections::get_connection,
            commands::connections::save_connection,
            commands::connections::delete_connection,
            commands::connections::test_connection,
            commands::connections::open_connection,
            commands::connections::close_connection,
            commands::connections::open_connection_ids,
            // projects
            commands::projects::list_projects,
            commands::projects::save_project,
            commands::projects::delete_project,
            commands::projects::move_connections,
            commands::projects::reorder_projects,
            // bookmarks
            commands::bookmarks::list_bookmarks,
            commands::bookmarks::add_bookmark,
            commands::bookmarks::remove_bookmark,
            commands::bookmarks::reorder_bookmarks,
            // database
            commands::db::db_children,
            commands::db::db_list_databases,
            commands::db::db_execute,
            commands::db::db_cancel,
            commands::db::db_table_data,
            commands::db::db_table_structure,
            commands::db::db_apply_changes,
            commands::db::db_object_ddl,
            commands::db::db_schema_snapshot,
            commands::db::db_get_key,
            commands::db::db_set_key,
            commands::db::db_delete_key,
            // history & saved queries
            commands::history::history_list,
            commands::history::history_clear,
            commands::history::saved_queries_list,
            commands::history::saved_query_save,
            commands::history::saved_query_delete,
            // transfer
            commands::transfer::export_data,
            commands::transfer::import_data,
            commands::transfer::import_preview,
            commands::transfer::backup_database,
            commands::transfer::restore_database,
            commands::transfer::transfer_tables,
            // INFRA-COMMANDS (k8s, rabbitmq) — registered during integration
            // kubernetes
            infra::k8s::k8s_contexts,
            infra::k8s::k8s_connect,
            infra::k8s::k8s_disconnect,
            infra::k8s::k8s_namespaces,
            infra::k8s::k8s_list,
            infra::k8s::k8s_get,
            infra::k8s::k8s_get_yaml,
            infra::k8s::k8s_apply_yaml,
            infra::k8s::k8s_delete,
            infra::k8s::k8s_scale,
            infra::k8s::k8s_restart,
            infra::k8s::k8s_cronjob_suspend,
            infra::k8s::k8s_events_for,
            infra::k8s::k8s_cluster_overview,
            infra::k8s::k8s_metrics,
            infra::k8s::k8s_save_text,
            infra::k8s::streams::k8s_stream_stop,
            infra::k8s::streams::k8s_watch_start,
            infra::k8s::streams::k8s_exec_input,
            infra::k8s::streams::k8s_exec_resize,
            infra::k8s::streams::k8s_exec_stop,
            infra::k8s::streams::k8s_port_forward_list,
            infra::k8s::streams::k8s_port_forward_stop,
            infra::k8s::streams::k8s_exec_start,
            infra::k8s::streams::k8s_logs_start,
            infra::k8s::streams::k8s_port_forward_start,
            infra::k8s::helm::k8s_helm_releases,
            infra::k8s::helm::k8s_helm_release,
            // network (VPN / SSH tunnels)
            network::net_list,
            network::net_get,
            network::net_save,
            network::net_delete,
            network::net_import,
            network::net_parse,
            network::net_connect,
            network::net_disconnect,
            network::net_status,
            network::net_logs,
            network::net_tools,
            network::net_install_tool,
            network::net_ssh_config_hosts,
            network::net_traffic,
            network::net_helper_status,
            network::net_helper_install,
            network::net_helper_uninstall,
            // claude code workspace
            claude::runner::claude_info,
            claude::runner::claude_start,
            claude::runner::claude_send,
            claude::runner::claude_permission,
            claude::runner::claude_control,
            claude::runner::claude_stop,
            claude::runner::claude_run_info,
            claude::pty::pty_open,
            claude::pty::pty_write,
            claude::pty::pty_resize,
            claude::pty::pty_close,
            claude::review::claude_review,
            claude::sessions::claude_projects,
            claude::sessions::claude_sessions,
            claude::sessions::claude_transcript,
            claude::files::fs_list,
            claude::files::fs_find_files,
            claude::remote::remote_resolve_dir,
            claude::sftp::remote_paste_image,
            claude::sftp::remote_stage_files,
            claude::sftp::sftp_mkdir,
            claude::sftp::sftp_rename,
            claude::sftp::sftp_delete,
            claude::sftp::sftp_transfer,
            claude::sftp::sftp_cancel,
            claude::ssh_forward::ssh_forward_start,
            claude::ssh_forward::ssh_forward_stop,
            claude::ssh_forward::ssh_forward_list,
            claude::files::fs_preview,
            claude::files::fs_write_text,
            claude::open_external::fs_open_external,
            claude::clipboard::fs_copy_files,
            claude::clipboard::fs_paste,
            claude::git::git_status,
            claude::git::git_file_versions,
            claude::git::git_diff_text,
            claude::git::git_stage,
            claude::git::git_unstage,
            claude::git::git_discard,
            claude::git::git_discard_all,
            claude::git::git_commit,
            // phone notifications (agent hooks → ntfy)
            notify::notify_settings,
            notify::notify_save,
            notify::notify_new_topic,
            notify::notify_test,
            notify::agents_status,
            // remote access from the mobile app
            remote::remote_status,
            remote::remote_save,
            remote::remote_pair_start,
            remote::remote_pair_cancel,
            remote::remote_qr,
            remote::remote_pair_answer,
            remote::remote_devices,
            remote::remote_device_update,
            remote::remote_device_revoke,
            remote::remote_audit,
            remote::remote_reset_identity,
            // notes
            notes::notes_list,
            notes::note_save,
            notes::note_delete,
            // AI connections (providers for the AI workspace terminal)
            agent::agent_open,
            agent::agent_send,
            agent::agent_stop,
            agent::agent_approve,
            agent::agent_close,
            agent::agent_answer,
            agent::agent_attach,
            agent::agent_sessions,
            ai::ai_providers_list,
            ai::ai_agent_models,
            ai::ai_provider_save,
            ai::ai_provider_delete,
            ai::ai_provider_detect,
            // git tools
            gitx::commands::git_repos,
            gitx::commands::git_log,
            gitx::commands::git_show,
            gitx::commands::git_commit_file_versions,
            gitx::commands::git_commit_diff,
            gitx::commands::git_blame,
            gitx::commands::git_refs,
            gitx::commands::git_checkout,
            gitx::commands::git_branch_delete,
            gitx::commands::git_branch_create,
            gitx::commands::git_branch_rename,
            gitx::commands::git_merge,
            gitx::commands::git_abort,
            gitx::commands::git_tag_create,
            gitx::commands::git_tag_delete,
            gitx::commands::git_commit_action,
            gitx::commands::git_stash_list,
            gitx::commands::git_stash_save,
            gitx::commands::git_stash_apply,
            gitx::commands::git_stash_drop,
            gitx::commands::git_fetch,
            gitx::commands::git_pull,
            gitx::commands::git_push,
            // rabbitmq
            infra::rabbitmq::rmq_bindings,
            infra::rabbitmq::rmq_close_connection,
            infra::rabbitmq::rmq_create_binding,
            infra::rabbitmq::rmq_declare_exchange,
            infra::rabbitmq::rmq_declare_queue,
            infra::rabbitmq::rmq_delete_binding,
            infra::rabbitmq::rmq_delete_exchange,
            infra::rabbitmq::rmq_delete_queue,
            infra::rabbitmq::rmq_get,
            infra::rabbitmq::rmq_get_messages,
            infra::rabbitmq::rmq_list,
            infra::rabbitmq::rmq_list_page,
            infra::rabbitmq::rmq_nodes,
            infra::rabbitmq::rmq_overview,
            infra::rabbitmq::rmq_publish,
            infra::rabbitmq::rmq_purge_queue,
            infra::rabbitmq::rmq_request,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // macOS: files opened with DevCat (Finder, Dock icon).
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Opened { urls } = &event {
                opened::accept(app, urls.iter().filter_map(|u| u.to_file_path().ok()));
            }
            let _ = app;
            // Tear down VPNs / tunnels so no elevated openvpn or wg interface outlives the app.
            if let tauri::RunEvent::Exit = event {
                shutdown_backend();
            }
        });
}
