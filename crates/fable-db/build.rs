//! Rebuild when a migration is added or changed: `sqlx::migrate!` embeds the
//! directory at compile time, but cargo does not track new files on its own.

fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
