//! Tauri shell: owns the open document and exposes it to the webview as a set
//! of narrow commands.
//!
//! All the heavy lifting lives in `doccore`. This crate only translates between
//! the arena and the wire types the React tree consumes, which is why the tree
//! can page through a two-million-node document without the webview ever
//! holding more than a few hundred rows.

mod commands;
mod dto;

use tauri::Manager;

pub use commands::Store;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Store::default())
        .setup(|app| {
            // `bson-editor <path>` opens straight into the document, which is
            // what a viewer is for.
            if let Some(path) = std::env::args().nth(1) {
                commands::open_into(app.state::<Store>().inner(), &path);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::core_version,
            commands::open_file,
            commands::load_text,
            commands::list_tabs,
            commands::close_tab,
            commands::children,
            commands::node_value,
            commands::copy_subtree,
            commands::set_node_value,
            commands::rename_node,
            commands::insert_node,
            commands::paste_json,
            commands::replace_node,
            commands::remove_node,
            commands::reexpand,
            commands::set_expand_depth,
            commands::save_file,
            commands::export_text
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
