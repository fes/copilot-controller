# Disposable diagnostic worker

This module provides a one-request Windows worker, not a terminal action,
credential sandbox, or readiness/qualification signal. It has no built-in
production callbacks and never selects a caller-provided executable or shell.
The executable must supply a finite, static **diagnostic-only** callback
allowlist. Callbacks must validate their typed payloads, avoid protocol stdout,
and return only privacy-safe diagnostic data.

## Integration

Add this directory as a Rust module and provide these dependencies:

```toml
serde = { version = "1", features = ["derive"] }
serde_json = "1"
uuid = { version = "1", features = ["serde"] }

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.61", features = [
    "Win32_Foundation",
    "Win32_Security",
    "Win32_Security_Cryptography",
    "Win32_Storage_FileSystem",
    "Win32_System_Console",
    "Win32_System_IO",
    "Win32_System_JobObjects",
    "Win32_System_Pipes",
    "Win32_System_Threading",
] }
```

Construct a `DiagnosticAllowlist` from a `'static` slice of
`DiagnosticCallback::new(kind, function)` entries. Functions have the type
`fn(serde_json::Value) -> Result<serde_json::Value, DiagnosticCallbackError>`.
The registry rejects invalid or duplicate kinds; both coordinator and worker
must use the same allowlist. Do not register general command dispatchers.

Before normal argument handling or console attachment, route
`maybe_run_hidden_worker(std::env::args_os().skip(1), &allowlist)`. If it
returns `Some(result)`, exit immediately: zero for `Ok`, nonzero for `Err`.
Do not expose the hidden argument as a public command. An executable must make
this routing change itself; this directory does not modify its entry point.

Call `probe(&request, timeout, &cancelled, &allowlist)` with an `AtomicBool`
cancellation flag. `DiagnosticResult` contains the correlated worker lifetime
and callback payload. `WorkerError` contains a static kind, optional worker
lifetime, and `replacement_allowed`; its display never includes native or
callback strings. Callers must still validate typed report target/window/time
correlation and lifecycle state. A successful response must not change
`read_only`, `command_ready`, or qualification policy.

Admission checks the 1 ms–120 s timeout, prelaunch cancellation, request and
allowlist, and bounded encoding before platform dispatch. Non-Windows returns
`unsupported_platform` after those checks without invoking a callback.
Frames use one 65,536-byte-maximum JSON body and require EOF; stderr is
discarded with a 16,384-byte limit.

On Windows the service creates private standard pipes with noninheritable
parent ends, enables inheritance only for child ends, restricts the inheritance
list, creates the current executable suspended and detached, assigns it to a
one-process kill-on-close Job, and checks its retained PID/creation time and
request nonce. I/O is concurrent. Failure cleanup waits
up to five seconds for exit and one additional second for I/O joining; an
unconfirmed result permanently blocks further probes in that supervisor
process. These budgets cannot physically bound a stuck OS or driver. The
fence is neither durable nor cross-process, and the Job is not a
credential-restricted hostile-code sandbox.

## Tests

`tests.rs` exercises framing, admission, correlation, cancellation, failure
precedence, and fencing with synthetic data. `tests/worker_fixture.rs` and
`tests/windows_integration.rs` are test-only, self-authored helpers: they
exercise the actual same-executable Windows path without vendor code or
hardware. The external harness can register them as the
`windows_worker_fixture` binary and `windows_integration` test; its library
crate name for the fixture is `windows_worker_harness`. Never add the fixture
callback to a production allowlist. Windows cross-checking compiles these
tests but does not execute them.
