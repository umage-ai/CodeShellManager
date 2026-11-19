//! Unix backend: portable-pty's openpty. The child is a session leader (portable-pty calls
//! `setsid`), so its process group is the tree: kill sends SIGKILL to the whole group.

use std::io;
use std::sync::{Arc, Mutex};

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use portable_pty::{CommandBuilder, MasterPty, native_pty_system};

use crate::{Backend, EarlyOutputBuffer, ExitCallback, ExitState, OutputSink, Pty, PtySize, SpawnOptions};

pub(crate) struct UnixPty {
    master: Mutex<Box<dyn MasterPty + Send>>,
    pid: Option<u32>,
    writer: Mutex<Option<std::sync::mpsc::Sender<Vec<u8>>>>,
    output: Arc<EarlyOutputBuffer>,
    exit: Arc<ExitState>,
}

fn pp_size(size: PtySize) -> portable_pty::PtySize {
    portable_pty::PtySize { rows: size.rows, cols: size.cols, pixel_width: 0, pixel_height: 0 }
}

pub(crate) fn spawn(opts: &SpawnOptions) -> io::Result<Arc<UnixPty>> {
    let pair = native_pty_system().openpty(pp_size(opts.size)).map_err(io::Error::other)?;
    let mut cmd = CommandBuilder::from_argv(opts.argv.clone());
    if let Some(cwd) = &opts.cwd {
        cmd.cwd(cwd);
    }
    for (k, v) in &opts.env {
        match v {
            Some(v) => cmd.env(k, v),
            None => cmd.env_remove(k),
        }
    }
    let mut child = pair.slave.spawn_command(cmd).map_err(io::Error::other)?;
    drop(pair.slave);
    let pid = child.process_id();
    let reader = pair.master.try_clone_reader().map_err(io::Error::other)?;
    let writer = pair.master.take_writer().map_err(io::Error::other)?;

    let output = Arc::new(EarlyOutputBuffer::new());
    let tag = pid.map_or_else(|| "?".to_string(), |p| p.to_string());
    crate::spawn_reader(format!("pty-read-{tag}"), reader, output.clone())?;
    let writer = crate::spawn_writer(format!("pty-write-{tag}"), writer)?;
    let exit = Arc::new(ExitState::default());
    let exit2 = exit.clone();
    std::thread::Builder::new().name(format!("pty-exit-{tag}")).spawn(move || {
        let code = child.wait().ok().map(|s| s.exit_code() as i32);
        exit2.latch(code);
    })?;
    Ok(Arc::new(UnixPty { master: Mutex::new(pair.master), pid, writer: Mutex::new(Some(writer)), output, exit }))
}

impl UnixPty {
    fn kill_tree(&self) {
        if let Some(pid) = self.pid.and_then(|p| i32::try_from(p).ok()) {
            if let Err(e) = killpg(Pid::from_raw(pid), Signal::SIGKILL) {
                log::debug!("killpg({pid}) failed: {e}");
            }
        }
    }
}

impl Pty for UnixPty {
    fn subscribe(&self, sink: OutputSink) {
        self.output.subscribe(sink);
    }
    fn on_exit(&self, cb: ExitCallback) {
        self.exit.on_exit(cb);
    }
    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        crate::send_to_writer(&self.writer, bytes)
    }
    fn resize(&self, size: PtySize) -> io::Result<()> {
        self.master
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .resize(pp_size(size))
            .map_err(io::Error::other)
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
    fn backend(&self) -> Backend {
        Backend::Unix
    }
    fn pid(&self) -> Option<u32> {
        self.pid
    }
}

impl Drop for UnixPty {
    fn drop(&mut self) {
        if !self.exit.has_exited() {
            self.kill_tree();
        }
        self.writer.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    }
}
