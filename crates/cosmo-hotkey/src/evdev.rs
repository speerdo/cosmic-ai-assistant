//! evdev and inotify plumbing. Every `unsafe` in the crate is an ioctl in
//! this file.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::fs::inotify;

const EV_KEY: u16 = 0x01;
const EV_MSC: u16 = 0x04;
/// `EVIOCSMASK`: `_IOW('E', 0x93, struct input_mask)`.
const EVIOCSMASK: u64 = (1 << 30) | (16 << 16) | ((b'E' as u64) << 8) | 0x93;
/// `EVIOCGBIT(EV_KEY, 96)`: the key-capability bitmap, `(KEY_MAX + 1) / 8`.
const EVIOCGBIT_KEY: u64 = (2 << 30) | (96 << 16) | ((b'E' as u64) << 8) | (0x20 + EV_KEY as u64);
/// First `BTN_*` code: buttons are never a trigger.
const BTN_MISC: u16 = 0x100;

#[repr(C)]
struct InputMask {
    type_: u32,
    codes_size: u32,
    codes_ptr: u64,
}

const EVENT_SIZE: usize = 24; // struct input_event on 64-bit Linux

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) struct Device {
    pub(crate) path: PathBuf,
    pub(crate) id: u64,
    file: File,
    code: u16,
}

impl Device {
    /// Open `path` if it can emit `code`; mask it down to that code.
    /// `Ok(None)`: a readable device without the key.
    pub(crate) fn open(path: &Path, code: u16) -> std::io::Result<Option<Self>> {
        if code >= BTN_MISC {
            return Ok(None);
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)?;
        if !file.metadata()?.file_type().is_char_device() {
            return Ok(None);
        }
        let mut bits = [0u8; 96];
        // SAFETY: EVIOCGBIT writes at most 96 bytes into `bits`.
        if unsafe { libc::ioctl(file.as_raw_fd(), EVIOCGBIT_KEY as _, bits.as_mut_ptr()) } < 0 {
            return Ok(None);
        }
        if bits[code as usize / 8] & (1 << (code % 8)) == 0 {
            return Ok(None);
        }
        set_masks(&file, code)?;
        Ok(Some(Self {
            path: path.to_owned(),
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            file,
            code,
        }))
    }

    /// Drain pending events; return the trigger's `EV_KEY` values in order.
    /// `Err` means the device is gone (ENODEV) or broken.
    pub(crate) fn read_edges(&mut self) -> std::io::Result<Vec<i32>> {
        let mut out = Vec::new();
        let mut buf = [0u8; EVENT_SIZE * 32];
        loop {
            match self.file.read(&mut buf) {
                Ok(0) => return Err(ErrorKind::UnexpectedEof.into()),
                Ok(n) => {
                    for ev in buf[..n].chunks_exact(EVENT_SIZE) {
                        let type_ = u16::from_ne_bytes([ev[16], ev[17]]);
                        let code = u16::from_ne_bytes([ev[18], ev[19]]);
                        let value = i32::from_ne_bytes([ev[20], ev[21], ev[22], ev[23]]);
                        // The mask already guarantees this; check anyway.
                        if type_ == EV_KEY && code == self.code {
                            out.push(value);
                        }
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(out),
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }
}

/// Deliver only `code` on `EV_KEY`, and nothing at all on `EV_MSC`.
fn set_masks(file: &File, code: u16) -> std::io::Result<()> {
    // The bitmap length must be a multiple of sizeof(long).
    let align = std::mem::size_of::<libc::c_long>();
    let len = (code as usize / 8 + 1).div_ceil(align) * align;
    let mut bits = vec![0u8; len];
    bits[code as usize / 8] = 1 << (code % 8);
    let key = InputMask {
        type_: u32::from(EV_KEY),
        codes_size: len as u32,
        codes_ptr: bits.as_ptr() as u64,
    };
    // A zero-length mask clears the type: no MSC_SCAN for any key.
    let msc = InputMask {
        type_: u32::from(EV_MSC),
        codes_size: 0,
        codes_ptr: 0,
    };
    for mask in [&key, &msc] {
        // SAFETY: `mask` and the bitmap it points to outlive the call; the
        // ioctl only reads them.
        if unsafe { libc::ioctl(file.as_raw_fd(), EVIOCSMASK as _, mask as *const InputMask) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// `/dev/input/event*`, sorted.
pub(crate) fn candidates() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir("/dev/input")
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("event"))
        })
        .collect();
    paths.sort();
    paths
}

/// inotify on `/dev/input`: any change means "rescan".
pub(crate) struct Hotplug {
    fd: OwnedFd,
}

impl Hotplug {
    pub(crate) fn new() -> std::io::Result<Self> {
        let fd = inotify::init(inotify::CreateFlags::NONBLOCK | inotify::CreateFlags::CLOEXEC)?;
        // CREATE for new nodes; ATTRIB because logind's uaccess ACL lands
        // just *after* the node appears, and only then can it be opened.
        inotify::add_watch(
            &fd,
            "/dev/input",
            inotify::WatchFlags::CREATE | inotify::WatchFlags::ATTRIB | inotify::WatchFlags::DELETE,
        )?;
        Ok(Self { fd })
    }

    pub(crate) fn drain(&self) {
        let mut buf = [std::mem::MaybeUninit::<u8>::uninit(); 4096];
        let mut reader = inotify::Reader::new(&self.fd, &mut buf);
        while reader.next().is_ok() {}
    }
}

pub(crate) struct Ready {
    pub(crate) hotplug: bool,
    pub(crate) devices: Vec<usize>,
}

pub(crate) fn poll(
    hotplug: &Hotplug,
    devices: &[Device],
    timeout: Duration,
) -> std::io::Result<Ready> {
    let mut fds = Vec::with_capacity(devices.len() + 1);
    fds.push(PollFd::new(&hotplug.fd, PollFlags::IN));
    for d in devices {
        fds.push(PollFd::new(&d.file, PollFlags::IN));
    }
    let ts = Timespec {
        tv_sec: timeout.as_secs() as _,
        tv_nsec: timeout.subsec_nanos() as _,
    };
    match rustix::event::poll(&mut fds, Some(&ts)) {
        Ok(_) => {}
        Err(rustix::io::Errno::INTR) => {
            return Ok(Ready {
                hotplug: false,
                devices: Vec::new(),
            });
        }
        Err(e) => return Err(e.into()),
    }
    let busy = |f: &PollFd<'_>| !f.revents().is_empty();
    Ok(Ready {
        hotplug: busy(&fds[0]),
        devices: fds[1..]
            .iter()
            .enumerate()
            .filter(|(_, f)| busy(f))
            .map(|(i, _)| i)
            .collect(),
    })
}
