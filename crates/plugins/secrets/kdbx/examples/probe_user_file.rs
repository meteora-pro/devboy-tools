//! Header-only probe of the user's KDBX file — no passphrase
//! needed, no values decrypted.
//!
//! Run with:
//!
//! ```sh
//! cargo run -p devboy-secret-kdbx --example probe_user_file -- /path/to/file.kdbx
//! ```
//!
//! Prints the detected KDBX version + file size. Used during the
//! K5 smoke step to confirm the target file is a recognised
//! KDBX 4 database before the user invests in typing the
//! passphrase into the GUI.

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "~/path/to/your.kdbx".to_owned());
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    println!("file: {path}");
    println!("size: {bytes} bytes");
    let mut f = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("open failed: {e}");
            std::process::exit(1);
        }
    };
    match keepass::Database::get_version(&mut f) {
        Ok(v) => println!("version: {v:?}"),
        Err(e) => {
            eprintln!("version probe failed: {e}");
            std::process::exit(2);
        }
    }
}
