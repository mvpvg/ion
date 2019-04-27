extern crate libc;
extern crate syscall;

use std::{
    env::{split_paths, var, vars},
    ffi::OsStr,
    fs::File,
    io::{self, prelude::*, BufReader, SeekFrom},
    mem,
    os::unix::{
        ffi::OsStrExt,
        fs::MetadataExt,
        io::{AsRawFd, RawFd},
    },
    path::PathBuf,
    process::exit,
    slice,
};
use syscall::{waitpid as waitpid_, SigAction};
pub use syscall::{
    wcoredump as wcoredump_, wexitstatus as wexitstatus_, wifcontinued as wifcontinued_,
    wifexited as wifexited_, wifsignaled as wifsignaled_, wifstopped as wifstopped_,
    wstopsig as wstopsig_, wtermsig as wtermsig_, ECHILD, EINTR,
};

pub const PATH_SEPARATOR: &str = ";";
pub const NULL_PATH: &str = "null:";

pub const O_CLOEXEC: usize = syscall::O_CLOEXEC;
pub const SIGHUP: i32 = syscall::SIGHUP as i32;
pub const SIGINT: i32 = syscall::SIGINT as i32;
pub const SIGTERM: i32 = syscall::SIGTERM as i32;
pub const SIGCONT: i32 = syscall::SIGCONT as i32;
pub const SIGSTOP: i32 = syscall::SIGSTOP as i32;
pub const SIGTSTP: i32 = syscall::SIGTSTP as i32;
pub const SIGPIPE: i32 = syscall::SIGPIPE as i32;
pub const WUNTRACED: i32 = syscall::WUNTRACED as i32;
pub const WNOHANG: i32 = syscall::WNOHANG as i32;
pub const WCONTINUED: i32 = syscall::WCONTINUED as i32;

pub const STDIN_FILENO: RawFd = 0;
pub const STDOUT_FILENO: RawFd = 1;
pub const STDERR_FILENO: RawFd = 2;

pub fn geteuid() -> io::Result<u32> { cvt(syscall::geteuid()).map(|pid| pid as u32) }

pub fn getuid() -> io::Result<u32> { cvt(syscall::getuid()).map(|pid| pid as u32) }

pub fn is_root() -> bool { syscall::geteuid().map(|id| id == 0).unwrap_or(false) }

pub unsafe fn fork() -> io::Result<u32> { cvt(syscall::clone(0)).map(|pid| pid as u32) }

pub fn fork_exit(status: i32) -> ! { exit(status) }

pub fn wexitstatus(status: i32) -> i32 { wexitstatus_(status as usize) as i32 }
pub fn wtermsig(status: i32) -> i32 { wtermsig_(status as usize) as i32 }
pub fn wstopsig(status: i32) -> i32 { wstopsig_(status as usize) as i32 }
pub fn wifcontinued(status: i32) -> bool { wifcontinued_(status as usize) }
pub fn wifsignaled(status: i32) -> bool { wifsignaled_(status as usize) }
pub fn wifstopped(status: i32) -> bool { wifstopped_(status as usize) }
pub fn wcoredump(status: i32) -> bool { wcoredump_(status as usize) }
pub fn wifexited(status: i32) -> bool { wifexited_(status as usize) }

pub fn waitpid(pid: i32, status: &mut i32, options: i32) -> Result<i32, i32> {
    let mut stat = 0;
    let result = match waitpid_(pid as usize, &mut stat, options as usize) {
        Err(ref error) => Err(error.errno),
        Ok(pid) => Ok(pid as i32),
    };

    *status = stat as i32;
    result
}

pub fn strerror(errno: i32) -> &'static str {
    syscall::error::STR_ERROR.get(errno as usize).map(|err| *err).unwrap_or("Unknown Error")
}

pub fn getpid() -> io::Result<u32> { cvt(syscall::getpid()).map(|pid| pid as u32) }

pub fn kill(pid: u32, signal: i32) -> io::Result<()> {
    cvt(syscall::kill(pid as usize, signal as usize)).and(Ok(()))
}

pub fn killpg(pgid: u32, signal: i32) -> io::Result<()> {
    cvt(syscall::kill(-(pgid as isize) as usize, signal as usize)).and(Ok(()))
}

pub fn pipe2(flags: usize) -> io::Result<(RawFd, RawFd)> {
    let mut fds = [0; 2];
    cvt(syscall::pipe2(&mut fds, flags))?;
    Ok((fds[0] as RawFd, fds[1] as RawFd))
}

pub fn setpgid(pid: u32, pgid: u32) -> io::Result<()> {
    cvt(syscall::setpgid(pid as usize, pgid as usize)).and(Ok(()))
}

pub fn fork_and_exec<F: Fn(), S: AsRef<str>>(
    prog: &str,
    args: &[S],
    stdin: Option<RawFd>,
    stdout: Option<RawFd>,
    stderr: Option<RawFd>,
    clear_env: bool,
    before_exec: F,
) -> io::Result<u32> {
    unsafe {
        match fork()? {
            0 => {
                if let Some(stdin) = stdin {
                    let _ = dup2(stdin, STDIN_FILENO);
                    let _ = close(stdin);
                }

                if let Some(stdout) = stdout {
                    let _ = dup2(stdout, STDOUT_FILENO);
                    let _ = close(stdout);
                }

                if let Some(stderr) = stderr {
                    let _ = dup2(stderr, STDERR_FILENO);
                    let _ = close(stderr);
                }

                before_exec();

                let error = execve(prog, args, clear_env);
                eprintln!("ion: command exec: {}", error);
                fork_exit(1);
            }
            pid => {
                if let Some(stdin) = stdin {
                    let _ = close(stdin);
                }

                if let Some(stdout) = stdout {
                    let _ = close(stdout);
                }

                if let Some(stderr) = stderr {
                    let _ = close(stderr);
                }

                Ok(pid)
            }
        }
    }
}

pub fn execve<S: AsRef<str>>(prog: &str, args: &[S], clear_env: bool) -> io::Error {
    // Get the PathBuf of the program if it exists.
    let prog = if prog.contains(':') || prog.contains('/') {
        // This is a fully specified scheme or path to an
        // executable.
        Some(PathBuf::from(prog))
    } else if let Ok(paths) = var("PATH") {
        // This is not a fully specified scheme or path.
        // Iterate through the possible paths in the
        // env var PATH that this executable may be found
        // in and return the first one found.
        split_paths(&paths)
            .filter_map(|mut path| {
                path.push(prog);
                if path.exists() {
                    Some(path)
                } else {
                    None
                }
            })
            .next()
    } else {
        None
    };

    if let Some(prog) = prog {
        let mut file = match File::open(&prog) {
            Ok(file) => file,
            Err(err) => return err,
        };

        // Construct a valid set of arguments to pass to execve. Ensure that
        // the interpreter program is the first argument, if any. Then comes
        // the program name. Finally all the arguments.
        let mut cvt_args: Vec<[usize; 2]> = Vec::with_capacity(args.len());

        // Check the interpreter.
        // `./test.ion` with `#!/bin/ion` should become:
        // /bin/ion ./test.ion <args...>
        let interpreter = {
            let mut reader = BufReader::new(&file);

            let mut shebang = [0; 2];
            let mut read = 0;
            while read < shebang.len() {
                match reader.read(&mut shebang[read..]) {
                    Ok(0) => break,
                    Ok(n) => read += n,
                    Err(err) => return err,
                }
            }

            if &shebang == b"#!" {
                // This should be interpreted.
                // Since fexec won't be called on the file itself but rather the interpreter,
                // we need to make sure ourselves the file is executable
                let metadata = match file.metadata() {
                    Ok(meta) => meta,
                    Err(err) => return err,
                };

                let uid = match syscall::getuid() {
                    Ok(uid) => uid,
                    Err(err) => return io::Error::from_raw_os_error(err.errno),
                };
                let gid = match syscall::getgid() {
                    Ok(gid) => gid,
                    Err(err) => return io::Error::from_raw_os_error(err.errno),
                };
                let mode = if uid == metadata.uid() as usize {
                    (metadata.mode() >> 3 * 2) & 0o7
                } else if gid == metadata.gid() as usize {
                    (metadata.mode() >> 3 * 1) & 0o7
                } else {
                    metadata.mode() & 0o7
                };

                if mode & 0o1 == 0o0 {
                    return io::ErrorKind::PermissionDenied.into();
                }

                let mut interpreter = Vec::new();
                match reader.read_until(b'\n', &mut interpreter) {
                    Ok(_) => {
                        if interpreter.ends_with(&[b'\n']) {
                            interpreter.pop().unwrap();
                        }
                        // TODO: When NLL becomes stable, reassign `file =`
                        // directly here instead of the `let interpreter = {`
                        // workaround.
                        // (But remember to make sure the vector lives long
                        // enough for the arguments!!)
                        Some(interpreter)
                    }
                    Err(err) => return err,
                }
            } else {
                match reader.seek(SeekFrom::Start(0)) {
                    Ok(_) => (),
                    Err(err) => return err,
                }
                None
            }
        };
        if let Some(ref interpreter) = interpreter {
            let path: &OsStr = OsStrExt::from_bytes(&interpreter);
            file = match File::open(path) {
                Ok(file) => file,
                Err(err) => return err,
            };
            cvt_args.push([interpreter.as_ptr() as usize, interpreter.len()]);
        }

        // Push the program name
        cvt_args.push([prog.as_os_str().as_bytes().as_ptr() as usize, prog.as_os_str().len()]);

        // Push all arguments
        for arg in args {
            let arg: &str = arg.as_ref();
            cvt_args.push([arg.as_ptr() as usize, arg.len()]);
        }

        // Push all environment variables
        let mut env_args: Vec<[usize; 2]> = Vec::new();
        let mut env_key_value: Vec<String> = Vec::new();
        if !clear_env {
            for (key, value) in vars() {
                env_key_value.push(key + "=" + &value);
            }
            // Can't use the same loop because pushing to a vector may reallocate.
            for env in &env_key_value {
                env_args.push([env.as_ptr() as usize, env.len()]);
            }
        }

        // Finally: Run the program!
        let error = syscall::fexec(file.as_raw_fd() as usize, &cvt_args, &env_args);
        io::Error::from_raw_os_error(error.err().unwrap().errno)
    } else {
        // The binary was not found.
        io::Error::from_raw_os_error(syscall::ENOENT)
    }
}

#[allow(dead_code)]
pub fn signal(signal: i32, handler: extern "C" fn(i32)) -> io::Result<()> {
    let new = SigAction {
        sa_handler: unsafe { mem::transmute(handler) },
        sa_mask:    [0; 2],
        sa_flags:   0,
    };
    cvt(syscall::sigaction(signal as usize, Some(&new), None)).and(Ok(()))
}

pub fn reset_signal(signal: i32) -> io::Result<()> {
    let new = SigAction {
        sa_handler: unsafe { mem::transmute(syscall::flag::SIG_DFL) },
        sa_mask:    [0; 2],
        sa_flags:   0,
    };
    cvt(syscall::sigaction(signal as usize, Some(&new), None)).and(Ok(()))
}

pub fn tcsetpgrp(tty_fd: RawFd, pgid: u32) -> io::Result<()> {
    let fd = cvt(syscall::dup(tty_fd as usize, b"pgrp"))?;

    let pgid_usize = pgid as usize;
    let res = syscall::write(fd, unsafe {
        slice::from_raw_parts(&pgid_usize as *const usize as *const u8, mem::size_of::<usize>())
    });

    let _ = syscall::close(fd);

    cvt(res).and(Ok(()))
}

pub fn dup(fd: RawFd) -> io::Result<RawFd> { cvt(syscall::dup(fd as usize, &[])).map(|fd| fd as RawFd) }

pub fn dup2(old: RawFd, new: RawFd) -> io::Result<RawFd> { cvt(syscall::dup2(old as usize, new as usize, &[])).map(|fd| fd as RawFd) }

pub fn close(fd: RawFd) -> io::Result<()> { cvt(syscall::close(fd as usize)).and(Ok(())) }

pub fn isatty(fd: RawFd) -> bool {
    if let Ok(tfd) = syscall::dup(fd as usize, b"termios") {
        let _ = syscall::close(tfd);
        true
    } else {
        false
    }
}

// Support function for converting syscall error to io error
fn cvt(result: Result<usize, syscall::Error>) -> io::Result<usize> {
    result.map_err(|err| io::Error::from_raw_os_error(err.errno))
}

// TODO
pub mod signals {
    pub fn block() {}

    /// Unblocks the SIGTSTP/SIGTTOU/SIGTTIN/SIGCHLD signals so children processes can be
    /// controlled
    /// by the shell.
    pub fn unblock() {}
}

pub mod variables {
    use super::libc::{self, c_char};

    pub fn get_user_home(_username: &str) -> Option<String> {
        // TODO
        None
    }

    pub fn get_host_name() -> Option<String> {
        let mut host_name = [0u8; 512];

        if unsafe { libc::gethostname(&mut host_name as *mut _ as *mut c_char, host_name.len()) }
            == 0
        {
            let len = host_name.iter().position(|i| *i == 0).unwrap_or(host_name.len());

            Some(unsafe { String::from_utf8_unchecked(host_name[..len].to_owned()) })
        } else {
            None
        }
    }
}

pub mod env {
    use std::{env, path::PathBuf};

    pub fn home_dir() -> Option<PathBuf> { env::var_os("HOME").map(PathBuf::from) }
}
