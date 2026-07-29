#![forbid(unsafe_code)]

fn main() {
    supercampus_observability::init("workflow-worker");
    println!("workflow-worker scaffold is ready");
}
