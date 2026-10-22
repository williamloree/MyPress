// Pas de console en version publiée sous Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    mypress_lib::run()
}
