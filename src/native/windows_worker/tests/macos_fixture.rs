//! Self-authored stdio fixture. Never enumerates or opens hardware.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn main() {
    use std::io::{Read, Write};

    use copilot_controller::native::windows_worker::{
        DiagnosticAllowlist, DiagnosticCallback, DiagnosticCallbackError, DiagnosticWorkerRoute,
        maybe_run_hidden_worker,
    };
    use serde_json::{Value, json};

    fn callback(payload: Value) -> Result<Value, DiagnosticCallbackError> {
        match std::env::var("MACOS_WORKER_FIXTURE_MODE")
            .unwrap_or_default()
            .as_str()
        {
            "parent-gate" => {
                // Constructor only: a different-executable test parent must
                // not confer native access, even with correct PGID and pipes.
                return Ok(json!({
                    "native_access_denied": copilot_controller::native::hid::native_backend().is_err(),
                }));
            }
            "stall" => std::thread::sleep(std::time::Duration::from_secs(60)),
            "stderr-exact" => std::io::stderr().write_all(&[b'x'; 16_384]).unwrap(),
            "stderr-over" => std::io::stderr().write_all(&[b'x'; 16_385]).unwrap(),
            "oversize" => return Ok(json!("x".repeat(128 * 1024))),
            "stdout-over" => {
                std::io::stdout()
                    .write_all(&65_536u32.to_le_bytes())
                    .unwrap();
                std::io::stdout().write_all(&[b'x'; 65_537]).unwrap();
            }
            _ => {}
        }
        Ok(payload)
    }

    static CALLBACKS: [DiagnosticCallback; 2] = [
        DiagnosticCallback::new("micro_hid_probe", callback),
        DiagnosticCallback::new("akp03_hid_probe", callback),
    ];
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let mode = std::env::var("MACOS_WORKER_FIXTURE_MODE").unwrap_or_default();
    if args.len() == 1 && args[0] == "round-trip" {
        use copilot_controller::native::windows_worker::{DiagnosticRequest, probe};
        let payload = json!({
            "capture_ms": 0, "query": false, "read_keymap": false,
            "test_pattern": false, "health_check": false,
        });
        let result = probe(
            &DiagnosticRequest {
                kind: "micro_hid_probe".into(),
                payload: payload.clone(),
            },
            std::time::Duration::from_secs(3),
            &std::sync::atomic::AtomicBool::new(false),
            &DiagnosticAllowlist::new(&CALLBACKS).unwrap(),
        )
        .unwrap();
        assert_eq!(result.payload, payload);
        assert_ne!(result.worker.pid, std::process::id());
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(result.worker.pid as i32, &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
        print!("{}", serde_json::to_string(&result).unwrap());
        return;
    }
    if mode == "no-read" {
        std::thread::sleep(std::time::Duration::from_secs(60));
        return;
    }
    if mode.starts_with("forge-") || mode == "trailing" {
        let mut frame = Vec::new();
        std::io::stdin()
            .take(65_541)
            .read_to_end(&mut frame)
            .unwrap();
        let request: Value = serde_json::from_slice(&frame[4..]).unwrap();
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
        let size = std::mem::size_of_val(&info) as i32;
        assert_eq!(
            unsafe {
                libc::proc_pidinfo(
                    libc::getpid(),
                    libc::PROC_PIDTBSDINFO,
                    0,
                    info.as_mut_ptr().cast(),
                    size,
                )
            },
            size
        );
        let info = unsafe { info.assume_init() };
        let mut response = json!({
            "version": 1, "request_nonce": request["request_nonce"],
            "worker": {
                "pid": std::process::id(),
                "creation_time_100ns": info.pbi_start_tvsec * 10_000_000 + info.pbi_start_tvusec * 10,
            },
            "outcome": {"status": "ok", "payload": {}},
        });
        match mode.as_str() {
            "forge-nonce" => response["request_nonce"] = json!(uuid::Uuid::new_v4()),
            "forge-pid" => response["worker"]["pid"] = json!(std::process::id() + 1),
            "forge-start" => response["worker"]["creation_time_100ns"] = json!(1),
            "forge-version" => response["version"] = json!(2),
            _ => {}
        }
        let body = serde_json::to_vec(&response).unwrap();
        std::io::stdout()
            .write_all(&(body.len() as u32).to_le_bytes())
            .unwrap();
        std::io::stdout().write_all(&body).unwrap();
        if mode == "trailing" {
            std::io::stdout().write_all(b"x").unwrap();
        }
        return;
    }
    let allowlist = if mode == "production" {
        copilot_controller::cli::diagnostic_allowlist().unwrap()
    } else if args.first().is_some_and(|arg| arg == "__akp03-worker") {
        static AKP03: [DiagnosticCallback; 1] = [CALLBACKS[1]];
        DiagnosticAllowlist::new_for_route(&AKP03, DiagnosticWorkerRoute::Akp03).unwrap()
    } else {
        DiagnosticAllowlist::new(&CALLBACKS).unwrap()
    };
    match maybe_run_hidden_worker(&args, &allowlist) {
        Some(Ok(())) => {}
        _ => std::process::exit(1),
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn main() {}
