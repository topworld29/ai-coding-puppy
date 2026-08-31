#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

fn main() {
    if golden_puppy_pet_lib::run_hook_bridge_if_requested() {
        return;
    }
    golden_puppy_pet_lib::run()
}
