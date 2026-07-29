#![forbid(unsafe_code)]

fn main() {
    supercampus_observability::init("notification-worker");
    println!("notification-worker scaffold is ready");
}
