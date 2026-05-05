#![cfg_attr(windows, windows_subsystem = "windows")]

mod behavior;
mod config;
mod manifest;
mod rust_behavior;
mod sprite_map;

#[cfg(target_os = "macos")]
mod assets;

#[cfg(target_os = "macos")]
mod wm;

#[cfg(target_os = "macos")]
mod macos;

fn main() {
    #[cfg(target_os = "macos")]
    macos::run();
}
