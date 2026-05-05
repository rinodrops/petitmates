#![cfg_attr(windows, windows_subsystem = "windows")]

mod behavior;
mod config;
mod engine;
mod manifest;
mod rust_behavior;
mod sprite_map;

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
