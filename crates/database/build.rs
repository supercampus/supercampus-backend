fn main() {
    // sqlx embeds runtime migrations in the compiled crate. Watching the directory
    // ensures adding a new migration invalidates incremental builds as well as edits.
    println!("cargo:rerun-if-changed=../../migrations/runtime");
}
