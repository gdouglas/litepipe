// sqlx::migrate! embeds the migrations directory when this crate compiles.
// Adding a migration file does not by itself make cargo recompile the crate,
// so a build could ship without its newest migration. This makes the
// directory part of the crate's inputs.
fn main() {
    println!("cargo:rerun-if-changed=src/migrations");
}
