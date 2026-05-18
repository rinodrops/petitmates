#![cfg_attr(windows, windows_subsystem = "windows")]

mod anim_trigger;
mod behavior;
mod config;
mod debug_menu;
mod demo_behavior;
mod engine;
mod manifest;
mod rust_behavior;
mod speech;
mod sprite_map;
mod user_config;
mod weather;

#[cfg(target_os = "macos")]
mod assets;

#[cfg(target_os = "macos")]
mod wm;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "windows")]
mod windows_wm;

#[cfg(target_os = "windows")]
mod windows_assets;

#[cfg(target_os = "windows")]
mod windows;

fn main() {
    #[cfg(target_os = "macos")]
    macos::run();

    #[cfg(target_os = "windows")]
    windows::run();
}
