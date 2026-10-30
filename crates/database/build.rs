// Re-embed migrations when a new .sql file is added (sqlx::migrate! is compile-time).
fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}
