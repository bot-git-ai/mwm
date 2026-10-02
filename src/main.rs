//! mwm — a small i3-inspired tiling window manager for macOS.

mod daemon;
mod keymap;
mod layout;
mod platform;
#[cfg(target_os = "macos")]
mod platform_darwin;
#[cfg(not(target_os = "macos"))]
mod platform_stub;
mod request;
mod types;

fn main() {
    let code = daemon::cli_main(std::env::args_os());
    std::process::exit(code);
}
