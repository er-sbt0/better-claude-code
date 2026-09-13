// The windowed binary writes nothing to a console on Windows; everything lives
// in the library so a mobile target could reuse it unchanged.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    desktop_lib::run()
}
