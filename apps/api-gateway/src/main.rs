#![forbid(unsafe_code)]

fn main() {
    supercampus_observability::init("api-gateway");
    println!("api-gateway scaffold is ready");
}
