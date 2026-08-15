//! The single component that spawns every subprocess AgentForge runs — SPEC.md §8,
//! docs/ARCHITECTURE.md §5.
//!
//! No other module calls `std::process::Command` directly.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chrono::Utc;
use thiserror::Error;

use crate::audit::AuditSink;
use crate::domain::exec::{CommandPolicy, ExecutionBudget, ProcessOutcome, ProcessSpec};
use crate::domain::AuditEvent;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to spawn process: {0}")]
    SpawnFailed(String),
    /// A requested spawn violated `CommandPolicy` (a denied/non-allowlisted program, or a cwd
    /// outside every configured allowed root) — SPEC.md §16's permission-policy layer. The
    /// process is never spawned; fail closed, not a warning.
    #[error("denied by policy: {0}")]
    PolicyDenied(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// A trait, not a bare struct, specifically so tests can substitute a spy/null implementation —
/// e.g. SPEC.md §18's requirement that `agentforge score` spawns zero processes is asserted via
/// an `Executor` that fails the test if invoked.
pub trait Executor: Send + Sync {
    fn spawn(
        &self,
        spec: &ProcessSpec,
        cwd: &Path,
        env_passthrough: &[String],
        budget: &ExecutionBudget,
        command_policy: &CommandPolicy,
        audit: &dyn AuditSink,
    ) -> Result<ProcessOutcome>;
}

/// The real implementation: `std::process::Command` plus a timeout watchdog.
///
/// Guarantees (SPEC.md §16): cwd is always set to the given path (validated to exist first —
/// never adapter-suppliable, see `ProcessSpec`'s doc); the child's environment is built from
/// exactly `env_passthrough` plus `spec.extra_env`, plus a small fixed set of OS-required
/// variables (`PATH`, and on Windows `SystemRoot`/`PATHEXT`/`COMSPEC`/`windir`) needed for the
/// operating system to locate and run *any* program at all — these carry no secrets and are not
/// policy-controlled, so passing them through unconditionally doesn't weaken the allowlist
/// guarantee for anything that actually matters; the directly-spawned process is killed on
/// timeout (a grandchild it detaches is only killed where the platform allows it); captured
/// output is truncated at `budget.max_output_bytes`; exactly one `ProcessSpawn` and one
/// `ProcessExit` audit event are emitted for every process actually spawned.
///
/// Before any of that: `spec.program` and `cwd` are checked against `command_policy` — a denied
/// or non-allowlisted program, or a cwd outside every configured allowed root, fails the spawn
/// closed (`Error::PolicyDenied`, zero `ProcessSpawn`/`ProcessExit` events, since nothing was
/// actually spawned) rather than proceeding with a warning. Every one of these checks — program
/// allow/deny, cwd containment, and the env-passthrough filter above — records a `PermissionCheck`
/// audit event, on both the allow and the deny path, so the audit log is a complete record of
/// every policy decision this call made, not just its failures.
pub struct SystemExecutor;

impl SystemExecutor {
    pub fn new() -> Self {
        SystemExecutor
    }
}

impl Default for SystemExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(windows)]
fn always_passthrough_vars() -> &'static [&'static str] {
    &["PATH", "SystemRoot", "PATHEXT", "COMSPEC", "windir"]
}

#[cfg(not(windows))]
fn always_passthrough_vars() -> &'static [&'static str] {
    &["PATH"]
}

const TRUNCATION_MARKER: &[u8] = b"\n...[truncated: exceeded max_output_bytes]...\n";

/// Process-tree containment for the timeout kill — SPEC.md §8 documents the intended guarantee
/// ("kills the child, and its process group / job object, where the platform provides one") but
/// until this module existed nothing actually created a process group or Job Object anywhere in
/// the codebase: `wait_with_timeout` only ever called `Child::kill()`, which terminates the one
/// directly-spawned process and leaves any grandchild it spawned and detached running forever —
/// a real gap for a tool whose entire purpose is running untrusted, timeout-bounded agent and
/// evaluator commands. `prepare` configures the child at spawn time (before it can create any
/// descendant of its own), `attach` captures whatever handle the platform needs to reach the
/// whole tree later, and `kill` uses it on timeout.
#[cfg(unix)]
mod tree {
    use std::process::{Child, Command};

    /// pgid 0: the child becomes the leader of a brand-new process group whose pgid equals its
    /// own pid — set before spawn so every process the child (or a descendant) forks inherits
    /// membership in it, unless that descendant explicitly calls `setpgid` to leave.
    pub fn prepare(command: &mut Command) {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    pub struct Handle;

    /// Always succeeds on Unix — there is no separate handle to acquire once `prepare` has set
    /// the process group at spawn time; `child.id()` (== the group's pgid) is enough at kill time.
    pub fn attach(_child: &Child) -> Option<Handle> {
        Some(Handle)
    }

    /// A negative pid targets the whole process group in a `kill(2)` call — one signal reaches
    /// the direct child and every descendant still in the group, not just the direct child alone.
    pub fn kill(child: &mut Child, _handle: &Option<Handle>) {
        let pid = child.id() as libc::pid_t;
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

#[cfg(windows)]
mod tree {
    use std::os::windows::io::AsRawHandle;
    use std::process::{Child, Command};
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    pub fn prepare(_command: &mut Command) {}

    /// Owns the Job Object handle. `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` means the OS itself kills
    /// every process still assigned to the job the moment the last handle to it closes — so even
    /// if AgentForge is itself killed mid-experiment (no `Drop` runs), the job (and every process
    /// in it) still dies rather than being orphaned. `Drop` here closes that last handle
    /// explicitly on the normal path (including a non-timeout exit that left a grandchild alive).
    pub struct Handle(windows_sys::Win32::Foundation::HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    /// Best-effort: if Job Object creation/assignment fails (e.g. an older Windows without
    /// nested-job support, while already running inside another job — rare but possible under
    /// some CI runners), returns `None` and the caller falls back to a plain `Child::kill()` of
    /// just the direct process, exactly the pre-existing behavior.
    pub fn attach(child: &Child) -> Option<Handle> {
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(info).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                CloseHandle(job);
                return None;
            }
            let assigned = AssignProcessToJobObject(job, child.as_raw_handle());
            if assigned == 0 {
                CloseHandle(job);
                return None;
            }
            Some(Handle(job))
        }
    }

    /// Terminates every process still in the job — the direct child and any grandchild it
    /// spawned that didn't escape the job, in one call, unlike `Child::kill()` alone.
    pub fn kill(_child: &mut Child, handle: &Option<Handle>) {
        if let Some(handle) = handle {
            unsafe {
                TerminateJobObject(handle.0, 1);
            }
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod tree {
    use std::process::{Child, Command};

    pub fn prepare(_command: &mut Command) {}
    pub struct Handle;
    pub fn attach(_child: &Child) -> Option<Handle> {
        None
    }
    pub fn kill(_child: &mut Child, _handle: &Option<Handle>) {}
}

impl Executor for SystemExecutor {
    fn spawn(
        &self,
        spec: &ProcessSpec,
        cwd: &Path,
        env_passthrough: &[String],
        budget: &ExecutionBudget,
        command_policy: &CommandPolicy,
        audit: &dyn AuditSink,
    ) -> Result<ProcessOutcome> {
        if spec.program.is_empty() {
            return Err(Error::SpawnFailed("program must not be empty".to_string()));
        }
        if !cwd.is_dir() {
            return Err(Error::SpawnFailed(format!(
                "cwd {} is not an existing directory",
                cwd.display()
            )));
        }

        // Policy checks run before anything is created on disk or spawned — a denied request
        // fails closed with zero side effects, not a warning followed by a best-effort attempt.
        check_and_audit_program_policy(command_policy, &spec.program, audit)?;
        check_and_audit_cwd_policy(command_policy, cwd, audit)?;
        audit_env_passthrough(env_passthrough, audit);

        let stdout_path = unique_capture_path("stdout");
        let stderr_path = unique_capture_path("stderr");
        let stdout_file = File::create(&stdout_path)?;
        let stderr_file = File::create(&stderr_path)?;

        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .current_dir(cwd)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        tree::prepare(&mut command);

        for key in env_passthrough.iter().map(String::as_str) {
            if let Ok(value) = std::env::var(key) {
                command.env(key, value);
            }
        }
        for key in always_passthrough_vars() {
            if let Ok(value) = std::env::var(key) {
                command.env(key, value);
            }
        }
        for (key, value) in &spec.extra_env {
            command.env(key, value);
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                // The two capture files above are already created at this point but never make
                // it into a `ProcessOutcome` for any caller to clean up (e.g. an unresolvable
                // `spec.program`, exercised for real by `broken_fake_adapter`-style fixtures and
                // any race participant whose adapter names a nonexistent binary) — without this,
                // every failed spawn attempt leaks two empty files into the OS temp dir forever.
                remove_file_best_effort(&stdout_path);
                remove_file_best_effort(&stderr_path);
                return Err(Error::SpawnFailed(format!("{}: {e}", spec.program)));
            }
        };

        // Piped, not redirected straight to the capture files (`Stdio::from(file)`): a direct
        // redirect lets the OS write the child's output to disk completely unbounded until the
        // process exits or is killed, so `budget.max_output_bytes` was previously enforced only
        // *after* the fact (`truncate_captured_file`, post-hoc `set_len`) — a runaway or hostile
        // command (an agent process, or a command embedded in an untrusted repo's evaluator
        // `setup_cmds`/`test_cmd`) could fill the OS temp dir with output before that ever ran,
        // regardless of the configured cap. Reading through a pipe on a dedicated thread per
        // stream and stopping the *write* to disk once `max_output_bytes` is reached (while still
        // draining the pipe so the child never blocks on a full pipe buffer) actually bounds disk
        // growth during capture, not just the final file size.
        let stdout_pipe = child.stdout.take().expect("stdout was piped");
        let stderr_pipe = child.stderr.take().expect("stderr was piped");
        let stdout_reader = spawn_capture_thread(stdout_pipe, stdout_file, budget.max_output_bytes);
        let stderr_reader = spawn_capture_thread(stderr_pipe, stderr_file, budget.max_output_bytes);

        let _ = audit.record(&AuditEvent::ProcessSpawn {
            command: spec.program.clone(),
            args: spec.args.clone(),
            cwd: cwd.to_path_buf(),
            at: Utc::now(),
        });

        // Captured immediately after spawn — before the child has had any real chance to fork
        // its own descendants — so the process-tree handle is ready the instant it's needed.
        let tree_handle = tree::attach(&child);
        let (exit_code, timed_out) = wait_with_timeout(
            &mut child,
            Duration::from_secs(budget.timeout_secs),
            &tree_handle,
        )?;
        // Dropped only once the child (and, on the timeout path, everything `tree::kill` could
        // reach) is done — on Windows this closes the Job Object handle, which itself kills any
        // still-living member of the job (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`) as a final
        // safety net even on a normal, non-timeout exit that left a grandchild behind. `Handle`
        // has no `Drop` impl on Unix/other (a plain marker there, see `tree` above), so this is a
        // documented no-op on those platforms rather than dead code — hence the lint override.
        #[allow(clippy::drop_non_drop)]
        drop(tree_handle);

        let _ = audit.record(&AuditEvent::ProcessExit {
            exit_code,
            timed_out,
            at: Utc::now(),
        });

        // The child (and everything `tree::kill` could reach) is fully dead by this point on
        // every path, so both pipes' write ends are closed and each reader thread has already
        // hit EOF or is a few bytes from it — these joins are not a wait for a live process.
        let _ = stdout_reader.join();
        let _ = stderr_reader.join();

        Ok(ProcessOutcome {
            exit_code,
            timed_out,
            stdout_path,
            stderr_path,
        })
    }
}

/// `denied_programs` always wins over `allowed_programs` — a program listed in both is refused,
/// the safer of the two possible readings when a policy is internally inconsistent.
fn program_policy_violation(policy: &CommandPolicy, program: &str) -> Option<String> {
    if policy.denied_programs.iter().any(|p| p == program) {
        return Some(format!("program {program:?} is on policy.denied_programs"));
    }
    if !policy.allowed_programs.is_empty() && !policy.allowed_programs.iter().any(|p| p == program)
    {
        return Some(format!(
            "program {program:?} is not in policy.allowed_programs"
        ));
    }
    None
}

fn check_and_audit_program_policy(
    policy: &CommandPolicy,
    program: &str,
    audit: &dyn AuditSink,
) -> Result<()> {
    match program_policy_violation(policy, program) {
        Some(reason) => {
            let _ = audit.record(&AuditEvent::PermissionCheck {
                action: "spawn_program".to_string(),
                allowed: false,
                reason: reason.clone(),
                at: Utc::now(),
            });
            Err(Error::PolicyDenied(reason))
        }
        None => {
            let _ = audit.record(&AuditEvent::PermissionCheck {
                action: "spawn_program".to_string(),
                allowed: true,
                reason: format!("program {program:?} permitted by policy"),
                at: Utc::now(),
            });
            Ok(())
        }
    }
}

/// cwd is always the caller-assigned worktree path, never adapter-suppliable (SPEC.md §8) — this
/// check is defense-in-depth against AgentForge's own misconfiguration, not agent containment.
/// Empty `allowed_roots` means no restriction configured, so every cwd passes.
fn cwd_policy_violation(policy: &CommandPolicy, cwd: &Path) -> Option<String> {
    if policy.allowed_roots.is_empty() {
        return None;
    }
    let canon_cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let within = policy.allowed_roots.iter().any(|root| {
        root.canonicalize()
            .map(|r| canon_cwd.starts_with(&r))
            .unwrap_or(false)
    });
    if within {
        None
    } else {
        Some(format!(
            "cwd {} is outside every root in policy.allowed_roots",
            cwd.display()
        ))
    }
}

fn check_and_audit_cwd_policy(
    policy: &CommandPolicy,
    cwd: &Path,
    audit: &dyn AuditSink,
) -> Result<()> {
    match cwd_policy_violation(policy, cwd) {
        Some(reason) => {
            let _ = audit.record(&AuditEvent::PermissionCheck {
                action: "spawn_cwd".to_string(),
                allowed: false,
                reason: reason.clone(),
                at: Utc::now(),
            });
            Err(Error::PolicyDenied(reason))
        }
        None => {
            let _ = audit.record(&AuditEvent::PermissionCheck {
                action: "spawn_cwd".to_string(),
                allowed: true,
                reason: format!("cwd {} permitted by policy", cwd.display()),
                at: Utc::now(),
            });
            Ok(())
        }
    }
}

/// Env-var filtering never blocks a spawn — it only shapes the child's environment — so unlike
/// the program/cwd checks, this always records `allowed: true`; the audit value is knowing
/// exactly how many (and, implicitly via the env-dump captured output, which) host vars were
/// exposed, not a pass/fail decision.
fn audit_env_passthrough(env_passthrough: &[String], audit: &dyn AuditSink) {
    let _ = audit.record(&AuditEvent::PermissionCheck {
        action: "env_passthrough".to_string(),
        allowed: true,
        reason: format!(
            "{} host var(s) allowlisted for passthrough",
            env_passthrough.len()
        ),
        at: Utc::now(),
    });
}

/// Polls rather than blocking indefinitely — `std::process::Child` has no built-in wait-with-
/// timeout. On timeout, kills the whole process tree reachable via `tree_handle` (see the `tree`
/// module) in addition to the direct child, and reaps the direct child so it doesn't become a
/// zombie/orphan handle.
fn wait_with_timeout(
    child: &mut Child,
    timeout: Duration,
    tree_handle: &Option<tree::Handle>,
) -> Result<(Option<i32>, bool)> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((status.code(), false));
        }
        if start.elapsed() >= timeout {
            tree::kill(child, tree_handle);
            let _ = child.kill();
            let _ = child.wait();
            return Ok((None, true));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Best-effort delete for AgentForge's own disposable internal files (captured subprocess
/// output, git plumbing scratch files) — never a correctness signal, so a failure past the retry
/// budget is silently ignored, same as a plain `let _ = std::fs::remove_file(..)`. On Windows,
/// retries briefly on a sharing-violation-style `PermissionDenied`: under heavy concurrent I/O
/// (many git/evaluator/agent processes spawned at once, as `race`/the test suite both do),
/// another process — most commonly real-time antivirus scanning a just-closed file — can briefly
/// hold an exclusive handle on it, the same transient contention documented on
/// `git::GitRepo::run_git_env`'s write-side retry. Without this, that brief window silently loses
/// the delete and leaks the file rather than erroring.
pub fn remove_file_best_effort(path: &Path) {
    let max_attempts: u32 = if cfg!(windows) { 5 } else { 1 };
    for attempt in 1..=max_attempts {
        match std::fs::remove_file(path) {
            Ok(()) => return,
            Err(e)
                if cfg!(windows)
                    && e.kind() == std::io::ErrorKind::PermissionDenied
                    && attempt < max_attempts =>
            {
                std::thread::sleep(Duration::from_millis(20 * u64::from(attempt)));
            }
            Err(_) => return,
        }
    }
}

fn unique_capture_path(kind: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "agentforge-{kind}-{}",
        crate::domain::ids::new_id(Utc::now())
    ))
}

/// Drains `reader` on a dedicated thread for the lifetime of the child's corresponding pipe,
/// writing at most `max_bytes` of it to `file` (appending `TRUNCATION_MARKER` the moment that cap
/// is first crossed) and silently discarding everything after — this is what actually bounds disk
/// growth *during* capture rather than only the final file size. Draining past the cap (instead of
/// stopping the read loop once it's reached) is required, not optional: stopping would leave the
/// pipe's OS buffer to fill up and block the child's next write indefinitely, turning "we stopped
/// recording your output" into "we silently hung your process."
fn spawn_capture_thread(
    mut reader: impl Read + Send + 'static,
    mut file: File,
    max_bytes: u64,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut written = 0u64;
        let mut truncated = false;
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if written >= max_bytes {
                continue; // keep draining the pipe; nothing more is written to disk
            }
            let take = ((max_bytes - written) as usize).min(n);
            if file.write_all(&buf[..take]).is_err() {
                break;
            }
            written += take as u64;
            if take < n && !truncated {
                truncated = true;
                let _ = file.write_all(TRUNCATION_MARKER);
            }
        }
    })
}
