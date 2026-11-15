//! Runs the built binary, so the remembered library is checked under a temporary HOME, never the real one.
use std::process::Command;

#[test]
fn library_use_is_remembered_by_later_commands() {
    let home = tempfile::tempdir().unwrap();
    let chesslib = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_chesslib"))
            .args(args)
            .env("HOME", home.path())
            .env_remove("CHESSLIB_DB")
            .current_dir(home.path())
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()
    };
    std::fs::create_dir(home.path().join("Openings")).unwrap();
    // A relative folder is stored as the absolute library file inside it.
    let lib = home.path().canonicalize().unwrap().join("Openings/library.sqlite3");
    let used = chesslib(&["library", "use", "Openings"]);
    assert_eq!(std::path::Path::new(used["path"].as_str().unwrap()).canonicalize().unwrap(), lib);
    chesslib(&["folders", "create", "Najdorf"]);
    let other = home.path().join("other.sqlite3");
    assert_eq!(chesslib(&["folders", "list", "--db", other.to_str().unwrap()]).as_array().unwrap().len(), 0, "--db still wins");
    assert_eq!(chesslib(&["folders", "list"])[0]["name"], "Najdorf");
}
