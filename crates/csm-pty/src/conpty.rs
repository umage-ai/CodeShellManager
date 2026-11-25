//! Windows ConPTY binding: the ONLY module in the workspace allowed `unsafe`.
//!
//! Invariants carried over from the WPF app (CLAUDE.md "Bundled ConPTY"):
//! * `conpty.dll` is loaded by FULL PATH from the executable's folder, never by bare name (a bare
//!   name walks the DLL search order: current directory, PATH — a hijack). A relative path is
//!   refused outright. The inbox functions are bound from `System32\kernel32.dll`, also by full
//!   path. Dependencies of the loaded DLL are searched only in its own folder and System32.
//! * An HPCON is only valid with the implementation that created it, so every pseudo console
//!   keeps its [`Api`] and sends Resize/Close to the same one.
//! * Fallback is never "no terminal": a bundled `CreatePseudoConsole` failure retries on inbox;
//!   three consecutive failures switch every later terminal to inbox for the rest of the run.
//! * The process is created suspended, put in a Job Object with KILL_ON_JOB_CLOSE, then resumed,
//!   so the whole tree dies with the PTY.
//!
//! Every `unsafe` block has a `// SAFETY:` comment. Keep this file small enough to review in
//! one sitting.

use std::ffi::{CStr, OsStr, c_void};
use std::fs::File;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, FARPROC, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{COORD, HPCON};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
    GetExitCodeProcess, GetProcessId, INFINITE, InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION,
    ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

use crate::policy::{BundledDecision, BundledFailurePolicy, decide_bundled};
use crate::{Backend, EarlyOutputBuffer, ExitCallback, ExitState, OutputSink, Pty, PtySize, SpawnOptions, cmdline};

// ── Binding ──────────────────────────────────────────────────────────────────────────────

type CreateFn = unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> i32;
type ResizeFn = unsafe extern "system" fn(HPCON, COORD) -> i32;
type CloseFn = unsafe extern "system" fn(HPCON);

/// The three ConPTY entry points of one implementation. The module is never unloaded, so the
/// pointers stay valid for the life of the process.
struct Api {
    kind: Backend,
    create: CreateFn,
    resize: ResizeFn,
    close: CloseFn,
}

fn wide_nul(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

fn export(module: *mut c_void, name: &CStr) -> FARPROC {
    // SAFETY: `module` is a valid module handle returned by LoadLibraryExW and never freed;
    // `name` is a NUL-terminated C string that outlives the call.
    unsafe { GetProcAddress(module, name.as_ptr().cast()) }
}

/// Loads `path` (which must be absolute) and binds the ConPTY exports.
fn bind(path: &Path, kind: Backend) -> Result<Api, String> {
    if !path.is_absolute() {
        return Err(format!("refusing to load a relative path: {}", path.display()));
    }
    let wide = wide_nul(path.as_os_str());
    // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call; hFile must be null.
    // The flags limit the search for the DLL's own dependencies to its folder and System32.
    let module = unsafe { LoadLibraryExW(wide.as_ptr(), null_mut(), LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32) };
    if module.is_null() {
        return Err(format!("could not load {}: {}", path.display(), io::Error::last_os_error()));
    }
    let (Some(create), Some(resize), Some(close)) =
        (export(module, c"CreatePseudoConsole"), export(module, c"ResizePseudoConsole"), export(module, c"ClosePseudoConsole"))
    else {
        return Err(format!("{} lacks a ConPTY export", path.display()));
    };
    // SAFETY: these are the documented exports with exactly these signatures (consoleapi.h:
    // HRESULT CreatePseudoConsole(COORD, HANDLE, HANDLE, DWORD, HPCON*), HRESULT
    // ResizePseudoConsole(HPCON, COORD), void ClosePseudoConsole(HPCON)), stdcall/"system" ABI.
    // A FARPROC and these fn pointers have the same size and representation.
    let api = unsafe {
        Api {
            kind,
            create: std::mem::transmute::<unsafe extern "system" fn() -> isize, CreateFn>(create),
            resize: std::mem::transmute::<unsafe extern "system" fn() -> isize, ResizeFn>(resize),
            close: std::mem::transmute::<unsafe extern "system" fn() -> isize, CloseFn>(close),
        }
    };
    Ok(api)
}

fn system_dir() -> Result<PathBuf, String> {
    let mut buf = vec![0u16; 512];
    // SAFETY: the buffer is valid for `buf.len()` u16s; the call writes at most that many.
    let n = unsafe { GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if n == 0 || n >= buf.len() {
        return Err(format!("GetSystemDirectoryW failed: {}", io::Error::last_os_error()));
    }
    buf.truncate(n);
    Ok(PathBuf::from(String::from_utf16_lossy(&buf)))
}

// ── Process-wide selection ───────────────────────────────────────────────────────────────

static PREFER_BUNDLED: AtomicBool = AtomicBool::new(true);
static INBOX: OnceLock<Result<Arc<Api>, String>> = OnceLock::new();
static SELECTION: Mutex<Option<Selected>> = Mutex::new(None);

struct Selected {
    api: Arc<Api>,
    reason: String,
    failures: BundledFailurePolicy,
}

pub(crate) fn set_prefer_bundled(prefer: bool) {
    PREFER_BUNDLED.store(prefer, Ordering::Relaxed);
}

fn inbox() -> io::Result<Arc<Api>> {
    INBOX
        .get_or_init(|| {
            let k32 = system_dir()?.join("kernel32.dll");
            bind(&k32, Backend::InboxConPty).map(Arc::new)
        })
        .clone()
        .map_err(io::Error::other)
}

fn lock_selection() -> std::sync::MutexGuard<'static, Option<Selected>> {
    SELECTION.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn select() -> io::Result<(Arc<Api>, String)> {
    let mut sel = lock_selection();
    if let Some(s) = sel.as_ref() {
        return Ok((s.api.clone(), s.reason.clone()));
    }
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    let decision = decide_bundled(PREFER_BUNDLED.load(Ordering::Relaxed), exe_dir.as_deref(), Path::is_file);
    let (api, reason) = match decision {
        BundledDecision::Try { dll, reason } => match bind(&dll, Backend::BundledConPty) {
            Ok(api) => (Arc::new(api), format!("bundled ({reason}; loaded {})", dll.display())),
            Err(e) => (inbox()?, format!("inbox — bundled failed to load: {e}")),
        },
        BundledDecision::Skip { reason } => (inbox()?, format!("inbox — {reason}")),
    };
    log::info!("PTY ConPTY: {reason}");
    *sel = Some(Selected { api: api.clone(), reason: reason.clone(), failures: Default::default() });
    Ok((api, reason))
}

pub(crate) fn selection() -> Option<(Backend, String)> {
    match select() {
        Ok((api, reason)) => Some((api.kind, reason)),
        Err(e) => Some((Backend::InboxConPty, format!("no ConPTY available: {e}"))),
    }
}

fn note_bundled_result(ok: bool, detail: &str) {
    let mut sel = lock_selection();
    let Some(s) = sel.as_mut() else { return };
    if ok {
        s.failures.record_success();
        return;
    }
    let (count, demote) = s.failures.record_failure();
    log::warn!(
        "PTY ConPTY: {detail} (consecutive failure {count}/{}) — this terminal falls back to inbox",
        BundledFailurePolicy::FAILURES_BEFORE_DEMOTION
    );
    if demote
        && s.api.kind == Backend::BundledConPty
        && let Ok(inbox) = inbox()
    {
        s.api = inbox;
        s.reason = "inbox — bundled failed repeatedly".into();
        log::warn!("PTY ConPTY: inbox from now on — bundled failed repeatedly");
    }
}

// ── Owned resources ──────────────────────────────────────────────────────────────────────

/// Wraps a raw handle returned by a successful Win32 call.
fn owned(h: HANDLE) -> io::Result<OwnedHandle> {
    if h.is_null() || h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `h` is a valid handle we just received from the OS and own exclusively; ownership
    // moves into the OwnedHandle, which closes it exactly once.
    Ok(unsafe { OwnedHandle::from_raw_handle(h) })
}

/// A pseudo console plus the implementation that created it. Closing is idempotent and
/// resize-after-close is a no-op (the HPCON is taken out under the lock).
struct PseudoConsole {
    api: Arc<Api>,
    hpc: Mutex<Option<HPCON>>,
}

impl PseudoConsole {
    fn create(api: Arc<Api>, size: PtySize, input: &OwnedHandle, output: &OwnedHandle) -> Result<Self, i32> {
        let mut hpc: HPCON = 0;
        // SAFETY: the pipe handles are valid for the call (ConPTY duplicates what it keeps);
        // `hpc` is a valid out-pointer; `create` was bound to the real export.
        let hr = unsafe { (api.create)(coord(size), input.as_raw_handle(), output.as_raw_handle(), 0, &mut hpc) };
        if hr < 0 {
            return Err(hr);
        }
        Ok(Self { api, hpc: Mutex::new(Some(hpc)) })
    }

    fn raw(&self) -> Option<HPCON> {
        *self.hpc.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn resize(&self, size: PtySize) -> io::Result<()> {
        let guard = self.hpc.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(hpc) = *guard else { return Ok(()) };
        // SAFETY: `hpc` is open (still in the mutex, which close() empties first) and belongs to
        // `self.api`, the implementation that created it.
        let hr = unsafe { (self.api.resize)(hpc, coord(size)) };
        if hr < 0 {
            return Err(io::Error::other(format!("ResizePseudoConsole failed: 0x{hr:08X}")));
        }
        Ok(())
    }

    fn close(&self) {
        let taken = self.hpc.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        if let Some(hpc) = taken {
            // SAFETY: `hpc` was created by `self.api` and is closed exactly once (taken out).
            unsafe { (self.api.close)(hpc) };
        }
    }
}

impl Drop for PseudoConsole {
    fn drop(&mut self) {
        self.close();
    }
}

fn coord(size: PtySize) -> COORD {
    COORD { X: size.cols.min(i16::MAX as u16) as i16, Y: size.rows.min(i16::MAX as u16) as i16 }
}

/// A PROC_THREAD_ATTRIBUTE_LIST holding the pseudo console attribute.
struct AttrList {
    // usize-aligned backing storage (the list holds pointers).
    buf: Vec<usize>,
}

impl AttrList {
    fn with_pseudoconsole(hpc: HPCON) -> io::Result<Self> {
        let mut size: usize = 0;
        // SAFETY: the documented size query: null list, count 1; it fails with
        // ERROR_INSUFFICIENT_BUFFER and writes the required size.
        unsafe { InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size) };
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut list = Self { buf: vec![0usize; size.div_ceil(size_of::<usize>())] };
        // SAFETY: the buffer is at least `size` bytes and pointer-aligned.
        if unsafe { InitializeProcThreadAttributeList(list.ptr(), 1, 0, &mut size) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the list is initialized. For PSEUDOCONSOLE the HPCON value itself is passed as
        // lpValue (it is an opaque pointer-sized handle), with cbSize = size_of::<HPCON>().
        let ok = unsafe {
            UpdateProcThreadAttribute(
                list.ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                hpc as *const c_void,
                size_of::<HPCON>(),
                null_mut(),
                null(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(list)
    }

    fn ptr(&mut self) -> *mut c_void {
        self.buf.as_mut_ptr().cast()
    }
}

impl Drop for AttrList {
    fn drop(&mut self) {
        // SAFETY: only reachable after a successful InitializeProcThreadAttributeList.
        unsafe { DeleteProcThreadAttributeList(self.ptr()) };
    }
}

fn create_pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let (mut r, mut w): (HANDLE, HANDLE) = (null_mut(), null_mut());
    // SAFETY: valid out-pointers; default (non-inheritable) security; default buffer size.
    if unsafe { CreatePipe(&mut r, &mut w, null(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((owned(r)?, owned(w)?))
}

/// A Job Object that kills every process in it when its last handle closes.
fn create_kill_job() -> io::Result<OwnedHandle> {
    // SAFETY: null attributes and name are allowed.
    let job = owned(unsafe { CreateJobObjectW(null(), null()) })?;
    // SAFETY: all-zero is a valid value for this plain-data struct.
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: `info` is the struct this info class expects, with its exact size.
    let ok = unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&raw const info).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

/// Environment block: the current environment with `changes` applied, sorted
/// case-insensitively as CreateProcess requires, UTF-16, double-NUL terminated.
fn env_block(changes: &[(std::ffi::OsString, Option<std::ffi::OsString>)]) -> Vec<u16> {
    use std::collections::BTreeMap;
    let key = |k: &OsStr| k.to_string_lossy().to_uppercase();
    let mut vars: BTreeMap<String, (std::ffi::OsString, std::ffi::OsString)> = BTreeMap::new();
    for (k, v) in std::env::vars_os() {
        vars.insert(key(&k), (k, v));
    }
    for (k, v) in changes {
        match v {
            Some(v) => vars.insert(key(k), (k.clone(), v.clone())),
            None => vars.remove(&key(k)),
        };
    }
    let mut block = Vec::new();
    for (k, v) in vars.values() {
        if k.is_empty() || k.encode_wide().any(|c| c == 0 || c == u16::from(b'=')) || v.encode_wide().any(|c| c == 0) {
            continue;
        }
        block.extend(k.encode_wide());
        block.push(u16::from(b'='));
        block.extend(v.encode_wide());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

// ── The PTY ──────────────────────────────────────────────────────────────────────────────

pub(crate) struct WinPty {
    pcon: Arc<PseudoConsole>,
    process: OwnedHandle,
    job: Option<OwnedHandle>,
    pid: u32,
    writer: Mutex<Option<crate::WriterTx>>,
    output: Arc<EarlyOutputBuffer>,
    exit: Arc<ExitState>,
}

pub(crate) fn spawn(opts: &SpawnOptions) -> io::Result<Arc<WinPty>> {
    let program = opts.argv.first().ok_or_else(|| io::Error::other("empty argv"))?;
    // A PATH the caller set for the child is also where its program is looked up.
    let child_path = opts.env.iter().rev().find(|(k, _)| k.to_string_lossy().eq_ignore_ascii_case("PATH")).map(|(_, v)| v.clone());
    let path = match child_path {
        Some(set) => set,
        None => std::env::var_os("PATH"),
    };
    let app = crate::resolve::find_program(program, path.as_deref())?;

    let mut cmd_line = match &opts.verbatim_args {
        Some(tail) => {
            if opts.argv.len() > 1 {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "verbatim_args with more than one argv element"));
            }
            let program: Vec<u16> = app.as_os_str().encode_wide().collect();
            let tail: Vec<u16> = tail.encode_wide().collect();
            cmdline::build_command_line_verbatim(&program, &tail)
        }
        None => {
            let mut argv16: Vec<Vec<u16>> = Vec::with_capacity(opts.argv.len());
            argv16.push(app.as_os_str().encode_wide().collect());
            argv16.extend(opts.argv.iter().skip(1).map(|a| a.encode_wide().collect()));
            cmdline::build_command_line(&argv16)
        }
    }
    .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    log::info!("PTY CreateProcess {}", cmdline::display_command_line(&cmd_line));
    cmd_line.push(0);

    let (in_read, in_write) = create_pipe()?;
    let (out_read, out_write) = create_pipe()?;

    let (selected, _) = if opts.allow_bundled { select()? } else { (inbox()?, String::new()) };
    let pcon = match PseudoConsole::create(selected.clone(), opts.size, &in_read, &out_write) {
        Ok(p) => {
            if selected.kind == Backend::BundledConPty {
                note_bundled_result(true, "");
            }
            p
        }
        Err(hr) if selected.kind == Backend::BundledConPty => {
            note_bundled_result(false, &format!("bundled CreatePseudoConsole failed: 0x{hr:08X}"));
            PseudoConsole::create(inbox()?, opts.size, &in_read, &out_write)
                .map_err(|hr| io::Error::other(format!("CreatePseudoConsole failed: 0x{hr:08X}")))?
        }
        Err(hr) => return Err(io::Error::other(format!("CreatePseudoConsole failed: 0x{hr:08X}"))),
    };
    let pcon = Arc::new(pcon);
    let hpc = pcon.raw().ok_or_else(|| io::Error::other("pseudo console closed"))?;

    let mut attrs = AttrList::with_pseudoconsole(hpc)?;
    // SAFETY: all-zero is a valid value for this plain-data struct.
    let mut si: STARTUPINFOEXW = unsafe { zeroed() };
    si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    // Without this a child of a process whose own std handles are redirected (a test runner, a
    // service) inherits THOSE instead of the pseudo console's.
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    si.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
    si.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
    si.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
    si.lpAttributeList = attrs.ptr();

    let app16 = wide_nul(app.as_os_str());
    let env = env_block(&opts.env);
    let cwd16 = opts.cwd.as_ref().map(|c| wide_nul(c.as_os_str()));
    // SAFETY: all-zero is a valid value for this plain-data out struct.
    let mut pi: PROCESS_INFORMATION = unsafe { zeroed() };
    // SAFETY: every pointer is valid for the call: NUL-terminated app path, mutable
    // NUL-terminated command line, a double-NUL UTF-16 environment block (flagged with
    // CREATE_UNICODE_ENVIRONMENT), optional NUL-terminated cwd, a fully initialized
    // STARTUPINFOEXW (flagged with EXTENDED_STARTUPINFO_PRESENT) and an out struct. No handles
    // are inherited.
    let ok = unsafe {
        CreateProcessW(
            app16.as_ptr(),
            cmd_line.as_mut_ptr(),
            null(),
            null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
            env.as_ptr().cast(),
            cwd16.as_ref().map_or(null(), |c| c.as_ptr()),
            &si.StartupInfo,
            &mut pi,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    drop(attrs);
    let process = owned(pi.hProcess)?;
    let thread = owned(pi.hThread)?;

    // Job before resume, so no grandchild can escape it.
    let job = match create_kill_job() {
        Ok(job) => {
            // SAFETY: both handles are valid and owned by us.
            if unsafe { AssignProcessToJobObject(job.as_raw_handle(), process.as_raw_handle()) } == 0 {
                log::warn!("PTY AssignProcessToJobObject failed: {}", io::Error::last_os_error());
                None
            } else {
                Some(job)
            }
        }
        Err(e) => {
            log::warn!("PTY CreateJobObject failed: {e} — tree kill degrades to TerminateProcess");
            None
        }
    };
    // SAFETY: `thread` is the suspended primary thread we just created.
    if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
        let err = io::Error::last_os_error();
        // SAFETY: valid process handle we own.
        unsafe { TerminateProcess(process.as_raw_handle(), 1) };
        return Err(err);
    }
    drop(thread);

    // ConPTY now owns its own duplicates of these; close ours so the pipe can report EOF.
    drop(in_read);
    drop(out_write);

    // SAFETY: valid process handle.
    let pid = unsafe { GetProcessId(process.as_raw_handle()) };
    let output = Arc::new(EarlyOutputBuffer::new());
    crate::spawn_reader(format!("pty-read-{pid}"), File::from(out_read), output.clone())?;
    let writer = crate::spawn_writer(format!("pty-write-{pid}"), File::from(in_write))?;
    let exit = Arc::new(ExitState::default());
    spawn_exit_monitor(&process, pid, exit.clone())?;

    log::info!("PTY started pid={pid} on {} {}x{}", pcon.api.kind.label(), opts.size.cols, opts.size.rows);
    Ok(Arc::new(WinPty { pcon, process, job, pid, writer: Mutex::new(Some(writer)), output, exit }))
}

/// Waits on a DUPLICATE of the process handle, so dropping the PTY (which closes the original)
/// never closes a handle another thread is waiting on. Exited fires on every path, exactly once.
fn spawn_exit_monitor(process: &OwnedHandle, pid: u32, exit: Arc<ExitState>) -> io::Result<()> {
    let mut dup: HANDLE = null_mut();
    // SAFETY: valid source handle; pseudo-handle for the current process; valid out-pointer.
    let ok = unsafe { DuplicateHandle(GetCurrentProcess(), process.as_raw_handle(), GetCurrentProcess(), &mut dup, 0, 0, DUPLICATE_SAME_ACCESS) };
    if ok == 0 {
        log::warn!("PTY DuplicateHandle failed: {} — exit code unknown", io::Error::last_os_error());
        exit.latch(None);
        return Ok(());
    }
    let dup = owned(dup)?;
    std::thread::Builder::new().name(format!("pty-exit-{pid}")).spawn(move || {
        // SAFETY: `dup` is a valid process handle owned by this thread until it returns.
        unsafe { WaitForSingleObject(dup.as_raw_handle(), INFINITE) };
        let mut code: u32 = 0;
        // SAFETY: valid handle and out-pointer.
        let got = unsafe { GetExitCodeProcess(dup.as_raw_handle(), &mut code) } != 0;
        exit.latch(got.then_some(code as i32));
    })?;
    Ok(())
}

impl WinPty {
    fn kill_tree(&self) {
        if let Some(job) = &self.job {
            // SAFETY: valid job handle we own.
            unsafe { TerminateJobObject(job.as_raw_handle(), 1) };
        } else {
            // SAFETY: valid process handle we own.
            unsafe { TerminateProcess(self.process.as_raw_handle(), 1) };
        }
    }
}

impl Pty for WinPty {
    fn subscribe(&self, sink: OutputSink) {
        self.output.subscribe(sink);
    }

    fn on_exit(&self, cb: ExitCallback) {
        self.exit.on_exit(cb);
    }

    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        crate::send_to_writer(&self.writer, bytes)
    }

    fn write_reply(&self, bytes: &[u8]) -> io::Result<()> {
        crate::send_reply_to_writer(&self.writer, bytes)
    }

    fn resize(&self, size: PtySize) -> io::Result<()> {
        self.pcon.resize(size)
    }

    fn has_exited(&self) -> bool {
        self.exit.has_exited()
    }

    fn exit_code(&self) -> Option<i32> {
        self.exit.code()
    }

    fn kill(&self) {
        self.kill_tree();
    }

    fn close(&self) {
        // ClosePseudoConsole can block until the reader drains the output pipe, so it runs on
        // its own thread (as in Drop). Closing twice is harmless: the HPCON is taken once.
        let pcon = self.pcon.clone();
        let spawned = std::thread::Builder::new().name(format!("pty-close-{}", self.pid)).spawn(move || pcon.close());
        if let Err(e) = spawned {
            log::warn!("PTY close thread failed to start ({e}); killing instead");
            self.kill_tree();
        }
    }

    fn backend(&self) -> Backend {
        self.pcon.api.kind
    }

    fn pid(&self) -> Option<u32> {
        Some(self.pid)
    }
}

impl Drop for WinPty {
    fn drop(&mut self) {
        self.kill_tree();
        // Stop the writer thread.
        self.writer.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        // ClosePseudoConsole can block until the reader drains the output pipe; never do that on
        // the caller's (possibly UI) thread. The reader keeps draining, sees EOF, and ends.
        let pcon = self.pcon.clone();
        let spawned = std::thread::Builder::new().name(format!("pty-close-{}", self.pid)).spawn(move || pcon.close());
        if let Err(e) = spawned {
            log::warn!("PTY close thread failed to start ({e}); closing inline");
            self.pcon.close();
        }
        // `job` drops after this: KILL_ON_JOB_CLOSE is a second guarantee.
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

    /// Splits with the real Win32 parser.
    fn split(line: &[u16]) -> Vec<String> {
        let mut z = line.to_vec();
        z.push(0);
        let mut n = 0i32;
        // SAFETY: NUL-terminated input; valid out-pointer.
        let argv = unsafe { CommandLineToArgvW(z.as_ptr(), &mut n) };
        assert!(!argv.is_null());
        let mut out = Vec::new();
        for i in 0..n as usize {
            // SAFETY: CommandLineToArgvW returned `n` valid NUL-terminated strings.
            unsafe {
                let p = *argv.add(i);
                let mut len = 0;
                while *p.add(len) != 0 {
                    len += 1;
                }
                out.push(String::from_utf16_lossy(std::slice::from_raw_parts(p, len)));
            }
        }
        // SAFETY: the array was allocated by CommandLineToArgvW with LocalAlloc.
        unsafe { LocalFree(argv.cast()) };
        out
    }

    #[test]
    fn hostile_args_round_trip_through_command_line_to_argv() {
        let cases = [
            "plain",
            "",
            "with space",
            "trailing\\",
            "trail space\\",
            "quote\"inside",
            "\\\"leading",
            "$(id)",
            "a;b|c&d",
            "`backtick`",
            "%PATH%",
            "--flag=value with space",
            "multi\\\\\\\"slash",
            "tab\there",
            "new\nline",
            "unicode ✓ 日本",
        ];
        let mut argv: Vec<Vec<u16>> = vec!["C:\\Program Files\\x.exe".encode_utf16().collect()];
        argv.extend(cases.iter().map(|c| c.encode_utf16().collect()));
        let line = cmdline::build_command_line(&argv).unwrap();
        let back = split(&line);
        assert_eq!(back[0], "C:\\Program Files\\x.exe");
        assert_eq!(&back[1..], &cases.map(String::from)[..]);
    }

    #[test]
    fn env_block_is_sorted_and_double_nul_terminated() {
        let block = env_block(&[("CSM_TEST_B".into(), Some("2".into())), ("CSM_TEST_A".into(), Some("1".into()))]);
        assert_eq!(&block[block.len() - 2..], &[0, 0]);
        let text = String::from_utf16_lossy(&block);
        let a = text.find("CSM_TEST_A=1").unwrap();
        let b = text.find("CSM_TEST_B=2").unwrap();
        assert!(a < b);
    }

    #[test]
    fn relative_dll_path_is_refused() {
        let e = bind(Path::new("conpty.dll"), Backend::BundledConPty).err().unwrap();
        assert!(e.contains("relative"));
    }

    #[test]
    fn inbox_binds_from_system32() {
        let api = inbox().unwrap();
        assert_eq!(api.kind, Backend::InboxConPty);
    }
}
