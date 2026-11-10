//! B19: live-judge policy flags apply with or without `--judges`.
//!
//! Runs the real `catalog-process` binary: a bare `--judge-backend
//! baseten` must honor `--sample-rate`/`--max-calls` instead of
//! running a full uncapped live rebuild.

mod common;

use common::synced_db;
use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_catalog-process"))
}

#[test]
fn sample_rate_zero_judges_nothing_live() {
    let (_dir, db) = synced_db("b19-sample");
    std::env::set_var("BASETEN_API_KEY", "probe-dummy");
    let out = Command::new(bin())
        .arg("--db")
        .arg(&db)
        .arg("--judge-backend")
        .arg("baseten")
        .arg("--sample-rate")
        .arg("0")
        .output()
        .unwrap();
    std::env::remove_var("BASETEN_API_KEY");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "zero-sample run must succeed without network.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("judged 0 endpoints"),
        "expected a judged-0 line, got:\n{stdout}"
    );
}

#[test]
fn openrouter_backend_dispatches_without_network() {
    // Iter 30: bare `--judge-backend openrouter` reads its model from
    // OPENROUTER_JUDGE_MODEL and its key from the shared
    // RANKINGS_API_KEY. Zero-sample run proves dispatch + from_env
    // wiring without touching OpenRouter.
    let (_dir, db) = synced_db("openrouter-cli");
    std::env::set_var("OPENROUTER_JUDGE_MODEL", "probe/model");
    std::env::set_var("RANKINGS_API_KEY", "probe-dummy");
    let out = Command::new(bin())
        .arg("--db")
        .arg(&db)
        .arg("--judge-backend")
        .arg("openrouter")
        .arg("--sample-rate")
        .arg("0")
        .output()
        .unwrap();
    std::env::remove_var("OPENROUTER_JUDGE_MODEL");
    std::env::remove_var("RANKINGS_API_KEY");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "zero-sample run must succeed without network.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("judged 0 endpoints"),
        "expected a judged-0 line, got:\n{stdout}"
    );
}

#[test]
fn max_calls_cap_fails_closed_live() {
    let (_dir, db) = synced_db("b19-caps");
    std::env::set_var("BASETEN_API_KEY", "probe-dummy");
    let out = Command::new(bin())
        .arg("--db")
        .arg(&db)
        .arg("--judge-backend")
        .arg("baseten")
        .arg("--max-calls")
        .arg("1")
        .output()
        .unwrap();
    std::env::remove_var("BASETEN_API_KEY");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "cap breach must fail.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("over the max_calls cap"),
        "expected a cap-breach error, got:\nstdout: {stdout}\nstderr: {stderr}"
    );
}
