// Em release, não abrir uma consola extra no Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    pc_studio_receiver_lib::run()
}
