use alt_os::{sys, temp};
use std::{
    ffi::{CStr, CString},
    fs::File,
    io::{self, Write},
    mem::MaybeUninit,
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt, process::ExitStatusExt},
    },
    path::Path,
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

fn c(path: &Path) -> CString {
    CString::new(path.as_os_str().as_bytes()).unwrap()
}

fn open_at(directory: &File, name: &str, flags: sys::c_int) -> io::Result<File> {
    let name = CString::new(name).unwrap();
    // SAFETY: the name is NUL terminated; a returned descriptor is owned here
    let fd = unsafe { sys::openat(directory.as_raw_fd(), name.as_ptr(), flags | sys::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd was just returned by openat
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[test]
fn flock_excludes_a_second_descriptor() {
    let file = temp::NamedTempFile::new().unwrap();
    let first = file.reopen().unwrap();
    let second = file.reopen().unwrap();
    let busy = |result: io::Result<()>| result.unwrap_err().kind() == io::ErrorKind::WouldBlock;

    sys::flock(&first, sys::LOCK_EX).unwrap();
    assert!(busy(sys::flock(&second, sys::LOCK_EX | sys::LOCK_NB)));
    assert!(busy(sys::flock(&second, sys::LOCK_SH | sys::LOCK_NB)));
    sys::flock(&first, sys::LOCK_UN).unwrap();

    sys::flock(&second, sys::LOCK_SH | sys::LOCK_NB).unwrap();
    sys::flock(&first, sys::LOCK_SH | sys::LOCK_NB).unwrap();
    assert!(busy(sys::flock(&first, sys::LOCK_EX | sys::LOCK_NB)));
    // unlocked, not closed: a child another test forks may hold a copy of the
    // descriptor until it execs, and the lock lives as long as any copy
    sys::flock(&second, sys::LOCK_UN).unwrap();
    sys::flock(&first, sys::LOCK_EX | sys::LOCK_NB).unwrap();
}

#[test]
fn kill_signals_a_child_and_reports_a_missing_process() {
    let mut child = Command::new("sleep").arg("60").spawn().unwrap();
    let pid = child.id() as sys::pid_t;
    sys::kill(pid, 0).unwrap();
    sys::kill(pid, sys::SIGTERM).unwrap();
    assert_eq!(child.wait().unwrap().signal(), Some(sys::SIGTERM));
    // the child is reaped, so its pid names no process
    // ESRCH: no such process
    assert_eq!(sys::kill(pid, 0).unwrap_err().raw_os_error(), Some(3));

    let mut child = Command::new("sleep").arg("60").spawn().unwrap();
    sys::kill(child.id() as sys::pid_t, sys::SIGKILL).unwrap();
    assert_eq!(child.wait().unwrap().signal(), Some(sys::SIGKILL));
}

static CONTINUED: AtomicBool = AtomicBool::new(false);
extern "C" fn on_continue(_signal: sys::c_int) {
    CONTINUED.store(true, Ordering::SeqCst);
}

#[test]
fn signal_installs_a_handler() {
    let handler = on_continue as *const () as sys::sighandler_t;
    // SAFETY: the handler only stores to an atomic, which is signal-safe
    unsafe { sys::signal(sys::SIGCONT, handler) };
    sys::kill(std::process::id() as sys::pid_t, sys::SIGCONT).unwrap();
    let start = Instant::now();
    while !CONTINUED.load(Ordering::SeqCst) {
        assert!(start.elapsed() < Duration::from_secs(10), "handler not run");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn geteuid_matches_the_owner_of_a_new_file() {
    let file = temp::NamedTempFile::new().unwrap();
    assert_eq!(sys::geteuid(), file.as_file().metadata().unwrap().uid());
}

#[test]
fn gethostname_returns_a_name() {
    let mut name = [0u8; 256];
    // SAFETY: the buffer is valid for writes of its length
    assert_eq!(
        unsafe { sys::gethostname(name.as_mut_ptr().cast(), name.len()) },
        0
    );
    let name = CStr::from_bytes_until_nul(&name).unwrap();
    assert!(!name.to_bytes().is_empty());
}

#[test]
fn clear_errno_resets_the_last_error() {
    // SAFETY: closing an invalid descriptor only fails
    assert_eq!(unsafe { sys::close(-1) }, -1);
    assert_ne!(io::Error::last_os_error().raw_os_error(), Some(0));
    sys::clear_errno();
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(0));
}

#[test]
fn directory_relative_calls_see_files_links_and_directories() {
    let directory = temp::tempdir().unwrap();
    std::fs::write(directory.path().join("file"), b"twelve bytes").unwrap();
    std::fs::create_dir(directory.path().join("inner")).unwrap();
    std::os::unix::fs::symlink("file", directory.path().join("link")).unwrap();
    let root = File::open(directory.path()).unwrap();

    let stat = |name: &str| {
        let name = CString::new(name).unwrap();
        let mut stat = MaybeUninit::<sys::stat>::uninit();
        // SAFETY: the name is NUL terminated and `stat` is writable
        let result = unsafe {
            sys::fstatat(
                root.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                sys::AT_SYMLINK_NOFOLLOW,
            )
        };
        assert_eq!(result, 0, "{}", io::Error::last_os_error());
        // SAFETY: fstatat succeeded and filled the structure
        unsafe { stat.assume_init() }
    };
    let file = stat("file");
    assert_eq!(file.st_mode & sys::S_IFMT, sys::S_IFREG);
    assert_eq!(file.st_size, 12);
    assert_eq!(
        file.st_ino,
        std::fs::metadata(directory.path().join("file"))
            .unwrap()
            .ino()
    );
    assert_eq!(stat("inner").st_mode & sys::S_IFMT, sys::S_IFDIR);
    assert_eq!(stat("link").st_mode & sys::S_IFMT, sys::S_IFLNK);

    let mut target = [0u8; 64];
    let link = CString::new("link").unwrap();
    // SAFETY: the name is NUL terminated and the buffer is writable
    let length = unsafe {
        sys::readlinkat(
            root.as_raw_fd(),
            link.as_ptr(),
            target.as_mut_ptr().cast(),
            target.len(),
        )
    };
    assert_eq!(&target[..length as usize], b"file");

    // the open flags have their meaning: no symlink is followed, and a
    // regular file is not a directory
    assert!(open_at(&root, "file", sys::O_RDONLY | sys::O_NOFOLLOW).is_ok());
    assert!(open_at(&root, "link", sys::O_RDONLY).is_ok());
    assert!(open_at(&root, "link", sys::O_RDONLY | sys::O_NOFOLLOW).is_err());
    assert!(open_at(&root, "inner", sys::O_RDONLY | sys::O_DIRECTORY).is_ok());
    assert_eq!(
        open_at(&root, "file", sys::O_RDONLY | sys::O_DIRECTORY)
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotADirectory
    );

    let fd = open_at(&root, ".", sys::O_RDONLY | sys::O_DIRECTORY)
        .unwrap()
        .into_raw_fd();
    // SAFETY: fd is an open directory whose ownership moves to the stream
    let stream = unsafe { sys::fdopendir(fd) };
    assert!(!stream.is_null());
    let mut names = Vec::new();
    loop {
        // SAFETY: the stream is open
        let entry = unsafe { sys::readdir(stream) };
        if entry.is_null() {
            break;
        }
        // SAFETY: readdir returned an entry whose name is NUL terminated
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        names.push(name.to_str().unwrap().to_owned());
    }
    // SAFETY: the stream is open and closed once
    assert_eq!(unsafe { sys::closedir(stream) }, 0);
    names.sort();
    assert_eq!(names, [".", "..", "file", "inner", "link"]);
}

#[test]
fn nonblocking_open_of_a_fifo_returns_at_once() {
    let directory = temp::tempdir().unwrap();
    let path = directory.path().join("fifo");
    // SAFETY: the path is NUL terminated
    assert_eq!(unsafe { sys::mkfifo(c(&path).as_ptr(), 0o600) }, 0);
    use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
    assert!(std::fs::metadata(&path).unwrap().file_type().is_fifo());
    // without O_NONBLOCK this open waits for a writer forever
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(sys::O_NONBLOCK)
        .open(&path)
        .unwrap();
}

#[test]
fn close_on_exec_keeps_the_descriptor_out_of_a_child() {
    let directory = temp::tempdir().unwrap();
    std::fs::write(directory.path().join("file"), b"").unwrap();
    let root = File::open(directory.path()).unwrap();
    let kept = open_at(&root, "file", sys::O_RDONLY).unwrap();
    let listing = |fd: i32| {
        Command::new("sh")
            .arg("-c")
            .arg(format!("test -e /dev/fd/{fd}"))
            .status()
            .unwrap()
            .success()
    };
    // open_at always adds O_CLOEXEC, so the child must not see the descriptor
    assert!(!listing(kept.as_raw_fd()));
    let name = CString::new("file").unwrap();
    // SAFETY: the name is NUL terminated; the descriptor is owned below
    let inherited = unsafe { sys::openat(root.as_raw_fd(), name.as_ptr(), sys::O_RDONLY) };
    assert!(inherited >= 0);
    // SAFETY: the descriptor was just returned by openat
    let inherited = unsafe { File::from_raw_fd(inherited) };
    assert!(listing(inherited.as_raw_fd()));
}

#[test]
fn filesystem_statistics_are_filled() {
    let directory = temp::tempdir().unwrap();
    let root = File::open(directory.path()).unwrap();

    let mut value = MaybeUninit::<sys::statvfs>::uninit();
    // SAFETY: the descriptor is open and `value` is writable
    assert_eq!(
        unsafe { sys::fstatvfs(root.as_raw_fd(), value.as_mut_ptr()) },
        0
    );
    // SAFETY: fstatvfs succeeded and filled the structure
    let value = unsafe { value.assume_init() };
    assert!(value.f_bsize >= 512 && value.f_bsize.is_power_of_two());
    assert!(value.f_frsize >= 512 && value.f_frsize.is_power_of_two());
    assert!(value.f_blocks > 0);
    assert!(value.f_bfree <= value.f_blocks);
    assert!(value.f_namemax >= 255);

    let mut value = MaybeUninit::<sys::statfs>::uninit();
    // SAFETY: the descriptor is open and `value` is writable
    assert_eq!(
        unsafe { sys::fstatfs(root.as_raw_fd(), value.as_mut_ptr()) },
        0
    );
    // SAFETY: fstatfs succeeded and filled the structure
    let value = unsafe { value.assume_init() };
    assert!(value.f_blocks > 0 && value.f_bfree <= value.f_blocks);
    #[cfg(target_os = "macos")]
    {
        // SAFETY: the kernel NUL terminates the type name
        let kind = unsafe { CStr::from_ptr(value.f_fstypename.as_ptr()) };
        let kind = kind.to_str().unwrap();
        assert!(!kind.is_empty() && kind.bytes().all(|byte| byte.is_ascii_alphanumeric()));
        // SAFETY: the kernel NUL terminates the mount point
        let mount = unsafe { CStr::from_ptr(value.f_mntonname.as_ptr()) };
        assert!(mount.to_bytes().starts_with(b"/"));
    }
    #[cfg(target_os = "linux")]
    {
        assert!(value.f_type != 0);
        assert!(value.f_bsize >= 512 && value.f_namelen >= 255);
    }
}

#[test]
fn poll_reports_readable_data() {
    let (mut writer, reader) = std::os::unix::net::UnixStream::pair().unwrap();
    let mut fds = [sys::pollfd {
        fd: reader.as_raw_fd(),
        events: sys::POLLIN,
        revents: 0,
    }];
    // SAFETY: `fds` holds one valid entry
    assert_eq!(unsafe { sys::poll(fds.as_mut_ptr(), 1, 0) }, 0);
    writer.write_all(b"x").unwrap();
    // SAFETY: as above
    assert_eq!(unsafe { sys::poll(fds.as_mut_ptr(), 1, 10_000) }, 1);
    assert_eq!(fds[0].revents & sys::POLLIN, sys::POLLIN);
}

#[test]
fn setsockopt_accepts_a_receive_buffer_size() {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let size: sys::c_int = 4096;
    // SAFETY: the value pointer and length describe `size`
    let result = unsafe {
        sys::setsockopt(
            socket.as_raw_fd(),
            sys::SOL_SOCKET,
            sys::SO_RCVBUF,
            (&raw const size).cast(),
            size_of_val(&size) as sys::socklen_t,
        )
    };
    assert_eq!(result, 0, "{}", io::Error::last_os_error());
}

/// Runs in a child process started by `setrlimit_caps_open_files`: lowering
/// the limit in the test process itself would break the other tests.
#[test]
fn setrlimit_child() {
    if std::env::var_os("ALT_OS_SETRLIMIT_CHILD").is_none() {
        return;
    }
    let limit = sys::rlimit {
        rlim_cur: 32,
        rlim_max: 32,
    };
    // SAFETY: `limit` is a valid structure for the duration of the call
    assert_eq!(unsafe { sys::setrlimit(sys::RLIMIT_NOFILE, &limit) }, 0);
    let mut files = Vec::new();
    let error = loop {
        match File::open("/dev/null") {
            Ok(file) => files.push(file),
            Err(error) => break error,
        }
        assert!(files.len() <= 32, "limit not applied");
    };
    // EMFILE: too many open files
    assert_eq!(error.raw_os_error(), Some(24));
    drop(files);
    println!("limit reached");
}

#[test]
fn setrlimit_caps_open_files() {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "sys::setrlimit_child", "--nocapture"])
        .env("ALT_OS_SETRLIMIT_CHILD", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("limit reached"),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
